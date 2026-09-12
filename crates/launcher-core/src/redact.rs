//! Masking of secrets in text that is about to leave the process boundary.
//!
//! DSH prints `dsh web: http://127.0.0.1:<port>/?token=<secret>` once its
//! server is up, and the launcher forwards every streamed line to two places
//! that outlive the process: the Activity panel and the rolling launcher log.
//! The ready line is also the *functional* handoff — the same URL is opened in
//! the webview, where the token must survive. So redaction happens at the
//! boundary (display / persistence), never on the value the launcher uses.
//!
//! The scanner is deliberately blunt: a named key followed by a separator is
//! masked whole, with no attempt to judge whether the value "looks like" a real
//! secret. Over-masking costs a debugging detail; under-masking leaks a
//! credential into a file the user may sync to a cloud drive.
//!
//! Being hand-rolled keeps `launcher-core` free of a regex dependency, matching
//! the rest of the crate's parsing style.

use std::borrow::Cow;

/// What a masked value is replaced with. Length is not preserved on purpose —
/// the size of a secret is itself a hint.
pub const MASK: &str = "***";

/// Keys that name a secret, longest first so a longer key wins over a shorter
/// one starting at the same offset.
const SECRET_KEYS: &[&str] = &[
    "launchtoken",
    "access_token",
    "access-token",
    "refresh_token",
    "refresh-token",
    "client_secret",
    "client-secret",
    "id_token",
    "api_key",
    "api-key",
    "apikey",
    "password",
    "authorization",
    "token",
    "secret",
];

/// Replace the value of every recognised secret key with [`MASK`].
///
/// Returns the input untouched (borrowed, no allocation) when nothing matches.
pub fn redact_secrets(input: &str) -> Cow<'_, str> {
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let span = named_secret(input, &lower, i).or_else(|| prefixed_secret(input, &lower, i));
        match span {
            Some((start, end)) => {
                let buf = out.get_or_insert_with(|| String::with_capacity(input.len()));
                buf.push_str(&input[copied..start]);
                buf.push_str(MASK);
                copied = end;
                i = end;
            }
            None => i += 1,
        }
    }

    match out {
        Some(mut buf) => {
            buf.push_str(&input[copied..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(input),
    }
}

/// A `key = value` / `key: value` pair where `key` names a secret.
/// Returns the byte range of the **value**.
fn named_secret(input: &str, lower: &str, i: usize) -> Option<(usize, usize)> {
    let b = input.as_bytes();
    // A key must not be the tail of a longer word: `…launchtoken=` has `token`
    // inside it, but the char before is alphanumeric, so only the real key
    // matches.
    if i > 0 && b[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let rest = &lower[i..];
    let key = SECRET_KEYS.iter().find(|k| rest.starts_with(**k))?;

    let mut j = i + key.len();
    // A quoted (JSON) key puts a closing quote before the separator.
    if matches!(b.get(j), Some(b'"') | Some(b'\'')) {
        j += 1;
    }
    if !matches!(b.get(j), Some(b'=') | Some(b':')) {
        return None;
    }
    j += 1;
    while b.get(j) == Some(&b' ') {
        j += 1;
    }
    let quote = match b.get(j) {
        Some(&q @ (b'"' | b'\'')) => {
            j += 1;
            Some(q)
        }
        _ => None,
    };

    let start = j;
    let end = match quote {
        Some(q) => {
            let mut k = j;
            while let Some(&c) = b.get(k) {
                if c == q {
                    break;
                }
                k += 1;
            }
            k
        }
        None => unquoted_value(input, lower, start, *key == "authorization"),
    };

    (end > start).then_some((start, end))
}

/// The extent of an unquoted value at `start`.
///
/// `wide` (the `Authorization` header) tolerates one embedded space so
/// `Bearer <token>` is masked as a unit instead of leaving the token exposed
/// after the scheme; everything else stops at the first separator.
fn unquoted_value(input: &str, lower: &str, start: usize, wide: bool) -> usize {
    let b = input.as_bytes();
    let mut spaces = usize::from(wide);
    let mut k = start;
    while let Some(&c) = b.get(k) {
        if c == b'&' || c == b'"' || c == b'\'' {
            break;
        }
        if c.is_ascii_whitespace() {
            if spaces == 0 {
                break;
            }
            let mut m = k;
            while b.get(m).map_or(false, |c| c.is_ascii_whitespace()) {
                m += 1;
            }
            // Trailing spaces are not part of the value.
            if m >= b.len() || matches!(b[m], b'&' | b'"' | b'\'') {
                break;
            }
            spaces -= 1;
            k = m;
            continue;
        }
        if !wide && matches!(c, b',' | b'}' | b']' | b'>' | b')' | b';' | b'<') {
            break;
        }
        k += 1;
    }
    // A sentence may end right after the value (`…token=abc.`) — the full stop
    // is punctuation, not part of the secret. Dots *inside* a value (a JWT) are
    // kept, so only a trailing run is trimmed.
    while k > start && b[k - 1] == b'.' {
        k -= 1;
    }
    let _ = lower;
    k
}

/// Secrets that carry their own marker, with no `key=` in front: `Bearer <tok>`
/// and OpenAI-style `sk-…` keys.
fn prefixed_secret(input: &str, lower: &str, i: usize) -> Option<(usize, usize)> {
    let b = input.as_bytes();
    if i > 0 && b[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    let rest = &lower[i..];

    if rest.starts_with("bearer") {
        let mut j = i + "bearer".len();
        let spaces_start = j;
        while b.get(j).map_or(false, |c| c.is_ascii_whitespace()) {
            j += 1;
        }
        if j == spaces_start || j >= b.len() {
            return None;
        }
        let end = unquoted_value(input, lower, j, false);
        return (end > j).then_some((j, end));
    }

    if rest.starts_with("sk-") {
        let start = i;
        let mut k = i;
        while b.get(k).map_or(false, |c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_') {
            k += 1;
        }
        // Short `sk-` runs are words, not keys.
        if k - start >= 20 {
            return Some((start, k));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted(s: &str) -> String {
        redact_secrets(s).into_owned()
    }

    #[test]
    fn masks_the_ready_line_and_keeps_the_port() {
        let line = "dsh web: http://127.0.0.1:43120/?token=eyJhbGciOiJIUzI1NiJ9.abc";
        assert_eq!(
            redacted(line),
            "dsh web: http://127.0.0.1:43120/?token=***"
        );
    }

    #[test]
    fn masks_only_the_secret_query_param() {
        let line = "http://127.0.0.1:3000/?token=abc&profile=work";
        assert_eq!(redacted(line), "http://127.0.0.1:3000/?token=***&profile=work");
    }

    #[test]
    fn masks_a_camel_case_launch_token() {
        assert_eq!(redacted("launchToken=abc123"), "launchToken=***");
    }

    #[test]
    fn masks_json_fields_including_authorization() {
        let line = r#"{"api_key": "abc", "url": "https://x/mcp"}"#;
        assert_eq!(
            redacted(line),
            r#"{"api_key": "***", "url": "https://x/mcp"}"#
        );
    }

    #[test]
    fn masks_an_authorization_header_as_one_unit() {
        let line = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig";
        assert_eq!(redacted(line), "Authorization: ***");
    }

    #[test]
    fn masks_a_bare_bearer_token() {
        assert_eq!(redacted("using Bearer abc.def.ghi now"), "using Bearer *** now");
    }

    #[test]
    fn masks_an_env_style_assignment() {
        assert_eq!(
            redacted("KANBOARD_API_TOKEN=deadbeef"),
            "KANBOARD_API_TOKEN=***"
        );
    }

    #[test]
    fn masks_a_standalone_openai_key() {
        let key = format!("key is sk-{}", "a".repeat(40));
        assert_eq!(redacted(&key), "key is ***");
    }

    #[test]
    fn leaves_prose_and_similar_words_alone() {
        for line in [
            "the secret sauce is out",
            "tokenizer=cl100k_base loaded",
            "setting the Authorization header",
        ] {
            assert_eq!(redacted(line), line, "should not touch: {line}");
        }
    }

    #[test]
    fn real_secrets_with_internal_dots_are_masked_whole() {
        assert_eq!(
            redacted("token=eyJhbGci.eyJzdWIi.sig"),
            "token=***"
        );
    }

    #[test]
    fn trailing_punctuation_survives() {
        assert_eq!(
            redacted("see (http://127.0.0.1:3000/?token=abc)."),
            "see (http://127.0.0.1:3000/?token=***)."
        );
    }

    #[test]
    fn a_clean_line_is_borrowed_not_reallocated() {
        let line = "dsh web: http://127.0.0.1:43120/";
        assert!(matches!(redact_secrets(line), Cow::Borrowed(_)));
    }

    #[test]
    fn empty_input_is_fine() {
        assert_eq!(redacted(""), "");
    }
}
