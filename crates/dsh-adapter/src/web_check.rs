//! Whether the URL DSH printed actually serves the workspace.
//!
//! The launcher used to call a boot ready when the port accepted a connection.
//! A port that accepts says nothing about what answers on it: a window opening
//! `http://127.0.0.1:<port>/?token=…` can land on DSH's plain-text refusal —
//! `dsh web authentication required; reopen the URL printed by dsh web.` —
//! while Activity reports a healthy boot.
//!
//! This is one `GET` of the URL DSH printed, with redirects **not** followed,
//! read against the shapes `dsh-client-connection`'s `authorizeIndex` produces
//! for a root request (0.1.5-rc.2, `lib/index.js:386`):
//!
//! | answer | what it means |
//! |---|---|
//! | `303` | `GET /?token=…` with a matching token: DSH is about to send the window to clean `/` with the `dsh-auth-…` session cookie. |
//! | `200` | the root was served outright — browser auth is not in play (a build that prints a bare URL). |
//! | `401` + DSH's own sentence | the token in that URL was refused. |
//! | anything else | not an answer this module can read. |
//!
//! Only a `401` carrying DSH's sentence is a refusal; a refused connection, a
//! timeout, a `500`, a `403` from a DSH revision that gates differently, and a
//! `401` worded differently all stay [`WebVerdict::Unreadable`], because the
//! launcher must not announce a failure it cannot substantiate. The caller
//! fails open on that verdict. The check keys on status + body only: the
//! `content-type` DSH sends is recorded in `docs/dsh-contract-inventory.md`
//! rather than asserted here, where a header change would turn a real refusal
//! into silence.
//!
//! Two properties are deliberate and load-bearing:
//!
//! * **No redirect following.** The `303` *is* the answer being looked for.
//!   Following it would turn the token exchange into a plain page fetch — and
//!   because a bare `/` without the session cookie is refused by DSH too, it
//!   would manufacture a false refusal on a healthy boot.
//! * **No cookie jar.** The probe sends no cookies, so a refusal is DSH
//!   rejecting *this URL's token*, not some saved browser session. The reverse
//!   case exists and is not decidable from here: a browser holding a
//!   still-valid session cookie for the same authority could load a URL whose
//!   token was refused. That is why a refusal is reported as what DSH answered
//!   rather than as "the workspace will not open".

use std::time::Duration;

/// The sentence DSH writes into the body of its refusal. This exact wording is
/// what turns a `401` into a verdict instead of a guess.
const REFUSAL_MARKER: &str = "dsh web authentication required";

/// Longest detail kept for a message. A refusal body is one line; this only
/// bounds what a non-DSH server could put there.
const DETAIL_LIMIT: usize = 200;

/// What one root request to the URL DSH printed came back as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebVerdict {
    /// `303`: DSH accepted the token in the URL and handed over (or found) the
    /// session cookie. A window opening this URL gets the app.
    Authenticated,
    /// `200`: the root was served. The strongest signal there is when the URL
    /// carries no token to exchange.
    Serving,
    /// `401` with DSH's own refusal sentence: the token in the URL was not
    /// accepted, so the window would show that sentence instead of the UI.
    Refused { status: u16, detail: String },
    /// No answer this module can read, and therefore no verdict.
    Unreadable { detail: String },
}

impl WebVerdict {
    /// True only when the URL is *known* to serve the app. The caller's
    /// readiness claim rests on this.
    pub fn serves_app(&self) -> bool {
        matches!(self, Self::Authenticated | Self::Serving)
    }

    /// True only when DSH itself answered that the token is refused.
    pub fn refused(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// Classify one root response. Pure, so the table in the module note is
/// testable without a server; `status` and `body` come off the wire in
/// [`check_root`].
pub fn classify(status: u16, body: &str) -> WebVerdict {
    match status {
        303 => WebVerdict::Authenticated,
        200 => WebVerdict::Serving,
        401 => {
            let detail = cap(body.trim());
            if detail.contains(REFUSAL_MARKER) {
                WebVerdict::Refused { status, detail }
            } else {
                WebVerdict::Unreadable {
                    detail: format!("401 with an unrecognised body ({detail})"),
                }
            }
        }
        other => WebVerdict::Unreadable {
            detail: format!("HTTP {other}"),
        },
    }
}

/// `GET` the URL DSH printed and classify the answer. Never fails: an
/// unreadable answer is a verdict, not an error.
pub async fn check_root(url: &str, timeout: Duration) -> WebVerdict {
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return WebVerdict::Unreadable {
                detail: format!("no HTTP client: {e}"),
            }
        }
    };
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(e) => {
            return WebVerdict::Unreadable {
                detail: short_error(&e),
            }
        }
    };
    // Only a refusal has a body worth reading; a `200` here is a whole index.html
    // and a `303` has none, so both are classified on the status alone.
    let status = response.status().as_u16();
    let body = if status == 401 {
        response.text().await.unwrap_or_default()
    } else {
        String::new()
    };
    classify(status, &body)
}

/// A one-line reason from a request error, with the URL kept out of it.
///
/// Not cosmetic: this string can reach a diagnosis the frontend renders
/// unredacted, and the URL carries the web token. `reqwest::Error`'s own
/// `Display` re-inserts that URL (`without_url` needs ownership of an error
/// that is not `Clone`), so the reason is rebuilt from the error's flags plus
/// its source chain — where the OS reason lives — and then put through the same
/// redactor the log sink uses.
fn short_error(e: &reqwest::Error) -> String {
    let reason = if e.is_timeout() {
        "no answer"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    let mut deepest: Option<String> = None;
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        deepest = Some(inner.to_string());
        source = inner.source();
    }
    let detail = match deepest {
        Some(inner) => format!("{reason}: {inner}"),
        None => reason.to_string(),
    };
    cap(&launcher_core::redact_secrets(&detail))
}

/// Truncate on a character boundary, marking that there was more.
fn cap(text: &str) -> String {
    if text.chars().count() <= DETAIL_LIMIT {
        return text.to_string();
    }
    let mut out: String = text.chars().take(DETAIL_LIMIT).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// DSH's refusal body, verbatim (`writeUnauthorized`).
    const REFUSAL_BODY: &str =
        "dsh web authentication required; reopen the URL printed by dsh web.\n";

    /// DSH's refusal response, verbatim headers.
    fn refusal() -> String {
        format!(
            "HTTP/1.1 401 Unauthorized\r\n\
             content-type: text/plain; charset=utf-8\r\n\
             cache-control: no-store\r\n\
             content-length: {}\r\n\
             connection: close\r\n\r\n{REFUSAL_BODY}",
            REFUSAL_BODY.len()
        )
    }

    /// The token exchange, verbatim shape (`authorizeIndex`, first branch).
    fn exchange() -> String {
        "HTTP/1.1 303 See Other\r\n\
         location: /\r\n\
         set-cookie: dsh-auth-abc123=v1.x.y; Max-Age=604800; Path=/; HttpOnly; SameSite=Strict\r\n\
         cache-control: no-store\r\n\
         content-length: 0\r\n\
         connection: close\r\n\r\n"
            .to_string()
    }

    async fn bind() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (
            listener,
            format!("http://127.0.0.1:{port}/?token=test-token"),
        )
    }

    /// Answer one connection with `response`, then close it.
    async fn serve(listener: &tokio::net::TcpListener, response: &str) {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    fn classify_401(body: &str) -> WebVerdict {
        classify(401, body)
    }

    #[test]
    fn dshs_own_sentence_is_a_refusal() {
        assert_eq!(
            classify_401("dsh web authentication required; reopen the URL printed by dsh web.\n"),
            WebVerdict::Refused {
                status: 401,
                detail: "dsh web authentication required; reopen the URL printed by dsh web."
                    .to_string(),
            }
        );
        assert!(classify_401("dsh web authentication required").refused());
    }

    #[test]
    fn a_401_without_dshs_sentence_is_not_a_verdict() {
        // A proxy, a different DSH revision, a different gate: the launcher has
        // no standing to call this a refusal.
        let verdict = classify_401("authentication required\n");
        assert!(matches!(verdict, WebVerdict::Unreadable { .. }));
        assert!(!verdict.refused());
        assert_eq!(
            classify(401, ""),
            WebVerdict::Unreadable {
                detail: "401 with an unrecognised body ()".to_string()
            }
        );
    }

    #[test]
    fn the_two_healthy_shapes_are_healthy() {
        assert_eq!(classify(303, ""), WebVerdict::Authenticated);
        // A 303 carries no body, but a body must not change the verdict.
        assert_eq!(classify(303, "See Other"), WebVerdict::Authenticated);
        assert_eq!(classify(200, ""), WebVerdict::Serving);
        assert!(classify(303, "").serves_app());
        assert!(classify(200, "<!doctype html>").serves_app());
    }

    #[test]
    fn anything_else_is_unreadable_not_fatal() {
        for status in [0u16, 204, 301, 302, 403, 404, 500, 502] {
            let verdict = classify(status, "whatever");
            assert!(
                matches!(verdict, WebVerdict::Unreadable { .. }),
                "HTTP {status} must not be a verdict, got {verdict:?}"
            );
            assert!(!verdict.serves_app() && !verdict.refused());
        }
    }

    #[test]
    fn a_flooded_body_is_capped() {
        let body = format!("dsh web authentication required{}", "x".repeat(5000));
        match classify_401(&body) {
            WebVerdict::Refused { detail, .. } => {
                assert_eq!(detail.chars().count(), DETAIL_LIMIT + 1);
                assert!(detail.ends_with('…'));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_token_exchange_is_read_as_authenticated() {
        let (listener, url) = bind().await;
        let server = tokio::spawn(async move {
            serve(&listener, &exchange()).await;
            // If the probe followed `location: /`, it arrives here without the
            // session cookie and DSH would refuse it — the false alarm that
            // `Policy::none()` and this test both exist to prevent.
            serve(&listener, &refusal()).await;
        });
        assert_eq!(
            check_root(&url, Duration::from_secs(5)).await,
            WebVerdict::Authenticated
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_refused_token_is_read_as_refused() {
        let (listener, url) = bind().await;
        let server = tokio::spawn(async move { serve(&listener, &refusal()).await });
        match check_root(&url, Duration::from_secs(5)).await {
            WebVerdict::Refused { status, detail } => {
                assert_eq!(status, 401);
                assert!(detail.contains(REFUSAL_MARKER), "got {detail:?}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn a_port_with_nothing_answerable_is_unreadable() {
        // A listener that accepts and says nothing: the request times out
        // rather than being refused, and either way there is no verdict.
        let (listener, url) = bind().await;
        let verdict = check_root(&url, Duration::from_millis(300)).await;
        assert!(
            matches!(verdict, WebVerdict::Unreadable { .. }),
            "got {verdict:?}"
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_dead_port_is_unreadable() {
        let (listener, url) = bind().await;
        drop(listener);
        let verdict = check_root(&url, Duration::from_secs(2)).await;
        match &verdict {
            // The URL carries the web token, so it must not be in the text that
            // can reach an unredacted diagnosis.
            WebVerdict::Unreadable { detail } => {
                assert!(
                    !detail.contains("test-token"),
                    "token leaked into {detail:?}"
                );
            }
            other => panic!("expected an unreadable answer, got {other:?}"),
        }
    }
}
