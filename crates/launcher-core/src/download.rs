//! Resumable HTTP file downloads with optional SHA-256 verification.
//!
//! The launcher's market/install paths fetch real files over HTTP (catalog
//! tarballs from China npm mirrors, bundled-artifact downloads, …). A plain
//! `GET` + `bytes()` restarts from zero every time a flaky connection drops
//! mid-body. This module instead:
//!
//! 1. streams chunks straight to a `<dest>.part` file as they arrive, so an
//!    interruption leaves a resumable partial instead of nothing;
//! 2. resumes on the next attempt by sending the partial's length as
//!    `Range: bytes=<offset>-` — the server answers `206` and we append,
//!    `200` means it ignored the range and we restart from scratch, and `416`
//!    means the partial already spans the whole file and we finalize it;
//! 3. verifies the completed file against an expected SHA-256 when one is
//!    supplied — a mismatch deletes the `.part` and errors, so a corrupted
//!    download can never be promoted over a good `dest`;
//! 4. only renames `.part` over `dest` after a clean EOF (and a passing
//!    checksum), so `dest` is never observed half-written.
//!
//! [`download_file`] never buffers the whole body in memory: the peak footprint
//! is one streamed chunk, which is what keeps resuming cheap enough to matter.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Bounded resumable attempts before giving up. Each retry continues from
/// where the previous one stopped, so this is bounded work, not repetition.
const MAX_ATTEMPTS: usize = 5;

/// Result of a completed [`download_file`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// Total bytes written to `dest`.
    pub bytes: u64,
    /// True when at least part of `dest` came from a pre-existing `.part`
    /// file (a `206` continuation or a `416`-finalized partial). False when
    /// the server sent the whole body from byte 0.
    pub resumed: bool,
    /// SHA-256 of the downloaded file, hex-encoded (always computed).
    pub sha256: String,
}

/// The sibling path a download is streamed to before it is promoted to `dest`.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// Download `url` to `dest`, resuming from any existing `<dest>.part`.
///
/// See the module docs for the resume/checksum semantics. On success `dest`
/// holds the complete, verified bytes and the `.part` is gone; on failure the
/// (possibly partial) `.part` is left in place so the caller can retry cheaply,
/// unless the failure was a checksum mismatch, which deletes the poisoned
/// partial.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
) -> Result<DownloadOutcome> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let part = part_path(dest);
    let mut offset = file_len(&part);
    let mut resumed = false;
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 1..=MAX_ATTEMPTS {
        let mut req = client.get(url);
        if offset > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let mut resp = match req.send().await {
            Ok(resp) => resp,
            Err(e) => {
                let err = anyhow!("GET {url} failed (attempt {attempt}/{MAX_ATTEMPTS}): {e}");
                // A connect failure with nothing cached is not helped by
                // resuming — fail fast. With a partial in hand a retry may win.
                if offset == 0 {
                    return Err(err);
                }
                last_err = Some(err);
                continue;
            }
        };

        match resp.status().as_u16() {
            200 => {
                // Server ignored the Range header (or none was sent): the full
                // body starts at byte 0, so any stale partial is irrelevant.
                offset = 0;
            }
            206 => {
                resumed = true;
            }
            416 => {
                // Range not satisfiable: the `.part` already spans the whole
                // file (a prior run finished but crashed before the rename).
                return finalize(&part, dest, expected_sha256, true, url);
            }
            status => {
                let err =
                    anyhow!("GET {url} failed (attempt {attempt}/{MAX_ATTEMPTS}): HTTP {status}");
                // A 4xx is permanent — surface it. Transient 5xx may clear up
                // and our Range header keeps whatever we already hold.
                if (400..500).contains(&status) {
                    return Err(err);
                }
                last_err = Some(err);
                continue;
            }
        }

        // Stream the body into the `.part` file, appending when we continued
        // a partial, truncating when we restarted from byte 0.
        let truncate = offset == 0;
        let mut file =
            open_writer(&part, truncate).with_context(|| format!("open {}", part.display()))?;
        let mut stream_err = None;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    file.write_all(&chunk)
                        .with_context(|| format!("write {}", part.display()))?;
                    offset += chunk.len() as u64;
                }
                Ok(None) => break,
                Err(e) => {
                    stream_err = Some(e);
                    break;
                }
            }
        }
        drop(file);

        match stream_err {
            None => return finalize(&part, dest, expected_sha256, resumed, url),
            Some(e) => {
                // Mid-body interruption — exactly the case resume exists for.
                // The partial is kept and `offset` already points past it, so
                // the next attempt continues with a fresh Range request.
                let err = anyhow!(
                    "GET {url} interrupted after {offset} bytes \
                     (attempt {attempt}/{MAX_ATTEMPTS}): {e}"
                );
                if attempt == MAX_ATTEMPTS {
                    return Err(err);
                }
                last_err = Some(err);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("download failed: {url}")))
}

fn open_writer(part: &Path, truncate: bool) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).write(true);
    if truncate {
        opts.truncate(true);
    } else {
        opts.append(true);
    }
    opts.open(part)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Promote a complete `.part` to `dest` after optionally verifying its SHA-256.
fn finalize(
    part: &Path,
    dest: &Path,
    expected_sha256: Option<&str>,
    resumed: bool,
    url: &str,
) -> Result<DownloadOutcome> {
    let sha256 = file_sha256(part).with_context(|| format!("hash {}", part.display()))?;
    if let Some(want) = expected_sha256.map(str::trim).filter(|s| !s.is_empty()) {
        if sha256 != want.to_ascii_lowercase() {
            // A poisoned partial must never masquerade as a good file — drop
            // it so the next attempt starts clean instead of appending to rot.
            let _ = std::fs::remove_file(part);
            return Err(anyhow!(
                "SHA-256 mismatch for {url}: expected {want}, got {sha256}"
            ));
        }
    }
    let bytes = file_len(part);
    if dest.exists() {
        std::fs::remove_file(dest).with_context(|| format!("replace {}", dest.display()))?;
    }
    std::fs::rename(part, dest)
        .with_context(|| format!("finalize {} → {}", part.display(), dest.display()))?;
    Ok(DownloadOutcome {
        bytes,
        resumed,
        sha256,
    })
}

/// Streaming SHA-256 of a file's bytes, hex-encoded lowercase.
pub fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// SHA-256 of in-memory bytes, hex-encoded lowercase. Used where content is
/// already buffered (e.g. a fetched `SKILL.md` body) rather than streamed to a
/// `.part` file — matching [`file_sha256`] on the same bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-download-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn test_body() -> Vec<u8> {
        b"0123456789abcdef".repeat(4096)
    }

    fn expected_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex_encode(&Sha256::digest(data))
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("build test client")
    }

    async fn bind() -> (tokio::net::TcpListener, std::net::SocketAddr) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    /// Read one HTTP/1.1 request head off the socket, returning the request
    /// line and the `Range:` header value (if any).
    async fn next_request(sock: &mut tokio::net::TcpStream) -> (String, Option<String>) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let request_line = head.lines().next().unwrap_or_default().to_string();
        let range = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .and_then(|l| l.split_once(':'))
            .map(|(_, value)| value.trim().to_string());
        (request_line, range)
    }

    fn range_offset(range: &str) -> u64 {
        range
            .trim_start_matches("bytes=")
            .split('-')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn downloads_and_verifies_sha256() {
        let (listener, addr) = bind().await;
        let bytes = test_body();
        let n = bytes.len();
        let expected = expected_hex(&bytes);
        let body = bytes.clone();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, range) = next_request(&mut sock).await;
            assert!(range.is_none(), "no Range on a fresh download");
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
        });

        let dir = temp_dir("ok");
        let dest = dir.join("artifact.tgz");
        let outcome = download_file(
            &test_client(),
            &format!("http://{addr}/x"),
            &dest,
            Some(&expected),
        )
        .await
        .expect("download succeeds");

        assert_eq!(outcome.sha256, expected);
        assert!(
            !outcome.resumed,
            "no partial existed, so nothing was resumed"
        );
        assert_eq!(outcome.bytes, n as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert!(!part_path(&dest).exists(), "partial promoted and removed");
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resumes_from_partial_after_interruption() {
        let (listener, addr) = bind().await;
        let bytes = test_body();
        let n = bytes.len();
        let expected = expected_hex(&bytes);
        let half = n / 2;
        let body = bytes.clone();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let server = tokio::spawn(async move {
            // First attempt: a full 200 content-length, but the connection is
            // closed after half the body — exactly the flaky-connection drop
            // the resume path must survive.
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, range) = next_request(&mut sock).await;
            seen2
                .lock()
                .unwrap()
                .push(range.clone().unwrap_or_default());
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body[..half]).await.unwrap();
            drop(sock);

            // Second attempt: a 206 resume request — serve the remainder from
            // the byte offset the downloader computed from its partial file.
            let (mut sock, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                    .await
                    .expect("a resume request must follow the interrupted transfer")
                    .unwrap();
            let (_req, range) = next_request(&mut sock).await;
            seen2
                .lock()
                .unwrap()
                .push(range.clone().unwrap_or_default());
            let offset = range.as_deref().map(range_offset).unwrap_or(0) as usize;
            let remainder = &body[offset..];
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {offset}-{}/{n}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                n - 1,
                remainder.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(remainder).await.unwrap();
        });

        let dir = temp_dir("resume");
        let dest = dir.join("artifact.tgz");
        let outcome = download_file(
            &test_client(),
            &format!("http://{addr}/x"),
            &dest,
            Some(&expected),
        )
        .await
        .expect("resume completes the download");

        assert!(outcome.resumed, "second attempt must continue the partial");
        assert_eq!(outcome.sha256, expected);
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);

        {
            let ranges = seen.lock().unwrap();
            assert_eq!(ranges[0], "", "first attempt sends no Range");
            assert_eq!(
                ranges[1],
                format!("bytes={half}-"),
                "second attempt resumes exactly where the drop happened"
            );
        }
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn checksum_mismatch_deletes_part_and_fails() {
        let (listener, addr) = bind().await;
        let bytes = test_body();
        let n = bytes.len();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, _range) = next_request(&mut sock).await;
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&bytes).await.unwrap();
        });

        let dir = temp_dir("bad");
        let dest = dir.join("artifact.tgz");
        let wrong = "0".repeat(64);
        let err = download_file(
            &test_client(),
            &format!("http://{addr}/x"),
            &dest,
            Some(&wrong),
        )
        .await
        .expect_err("checksum mismatch must fail");

        assert!(
            err.to_string().contains("SHA-256 mismatch"),
            "error names the checksum: {err}"
        );
        assert!(!dest.exists());
        assert!(
            !part_path(&dest).exists(),
            "poisoned partial must be removed, not left for a resume"
        );
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn server_ignoring_range_restarts_full() {
        let (listener, addr) = bind().await;
        let bytes = test_body();
        let n = bytes.len();
        let expected = expected_hex(&bytes);
        let dir = temp_dir("ignoring");
        let dest = dir.join("artifact.tgz");
        // A stale partial of the *wrong, shorter* content sits in the cache.
        std::fs::write(part_path(&dest), &bytes[..n / 4]).unwrap();
        let body = bytes.clone();
        let server = tokio::spawn(async move {
            // Deliberately never honors Range: every request gets a full 200.
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, _range) = next_request(&mut sock).await;
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
        });

        let outcome = download_file(
            &test_client(),
            &format!("http://{addr}/x"),
            &dest,
            Some(&expected),
        )
        .await
        .expect("full restart succeeds");

        assert!(!outcome.resumed, "a 200 means the partial was not reused");
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn range_not_satisfiable_finalizes_existing_part() {
        let (listener, addr) = bind().await;
        let bytes = test_body();
        let expected = expected_hex(&bytes);
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, range) = next_request(&mut sock).await;
            assert!(range.is_some(), "a full partial must be resumed via Range");
            sock.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let dir = temp_dir("complete");
        let dest = dir.join("artifact.tgz");
        // A prior run finished writing the whole file but crashed before the
        // promote-to-dest rename — the `.part` is already complete.
        std::fs::write(part_path(&dest), &bytes).unwrap();

        let outcome = download_file(
            &test_client(),
            &format!("http://{addr}/x"),
            &dest,
            Some(&expected),
        )
        .await
        .expect("416 finalizes the existing partial");

        assert!(outcome.resumed);
        assert_eq!(std::fs::read(&dest).unwrap(), bytes);
        assert!(!part_path(&dest).exists());
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn permanent_error_fails_without_leaving_part() {
        let (listener, addr) = bind().await;
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (_req, _range) = next_request(&mut sock).await;
            let head = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            sock.write_all(head.as_bytes()).await.unwrap();
        });

        let dir = temp_dir("err");
        let dest = dir.join("artifact.tgz");
        let err = download_file(
            &test_client(),
            &format!("http://{addr}/missing"),
            &dest,
            None,
        )
        .await
        .expect_err("404 must fail");

        assert!(err.to_string().contains("HTTP 404"));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn part_path_appends_dot_part_sibling() {
        assert_eq!(
            part_path(Path::new("/tmp/a/artifact.tgz")),
            PathBuf::from("/tmp/a/artifact.tgz.part")
        );
        assert_eq!(part_path(Path::new("plain")), PathBuf::from("plain.part"));
    }
}
