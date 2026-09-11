//! The GitHub fetch relay shared by every git/raw fetch in the launcher.
//!
//! github.com's git endpoint and `raw.githubusercontent.com` are both
//! unreliable from mainland China, so the Install Center's mirror toggle routes
//! fetches through the gh-proxy relay instead. Both the plugin path and the
//! skill path build that URL here, so the relay host is stated once and the
//! toggle cannot mean two different things.

/// Reverse-proxy prefix for GitHub fetches when the mirror is on. gh-proxy
/// relays git smart-HTTP and raw files alike, so the whole upstream URL is
/// appended verbatim (`https://gh-proxy.com/https://github.com/o/r.git`).
pub const GITHUB_MIRROR_BASE: &str = "https://gh-proxy.com/";

/// The relay form of a GitHub URL. Returned as-is for anything else, so a
/// caller can map a list of candidates without checking first.
pub fn mirror_url(url: &str) -> String {
    if url.starts_with("https://github.com/") || url.starts_with("https://raw.githubusercontent.com/")
    {
        format!("{GITHUB_MIRROR_BASE}{url}")
    } else {
        url.to_string()
    }
}

/// The URLs to try for one fetch — the preferred transport first, the other as
/// the fallback, so a fetch only fails when both do.
///
/// The toggle decides the *order*, not whether the relay may be used at all: a
/// user who turned the mirror on is telling us their network cannot reach
/// github.com, and a user who left it off still should not lose an install to a
/// single blocked transport.
pub fn fetch_candidates(url: &str, mirror: bool) -> Vec<String> {
    let direct = url.to_string();
    let mirrored = mirror_url(url);
    if mirrored == direct {
        return vec![direct];
    }
    if mirror {
        vec![mirrored, direct]
    } else {
        vec![direct, mirrored]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_url_prefixes_only_github_hosts() {
        assert_eq!(
            mirror_url("https://github.com/o/r.git"),
            "https://gh-proxy.com/https://github.com/o/r.git"
        );
        assert_eq!(
            mirror_url("https://raw.githubusercontent.com/o/r/HEAD/SKILL.md"),
            "https://gh-proxy.com/https://raw.githubusercontent.com/o/r/HEAD/SKILL.md"
        );
        // A gist or any other host is left alone rather than half-rewritten.
        assert_eq!(mirror_url("https://gist.github.com/o/1"), "https://gist.github.com/o/1");
        assert_eq!(mirror_url("https://officialskills.sh/o/r/s"), "https://officialskills.sh/o/r/s");
    }

    #[test]
    fn fetch_candidates_puts_the_toggle_first_and_keeps_a_fallback() {
        let direct = "https://github.com/o/r.git";
        assert_eq!(fetch_candidates(direct, false), [direct, "https://gh-proxy.com/https://github.com/o/r.git"]);
        assert_eq!(fetch_candidates(direct, true), ["https://gh-proxy.com/https://github.com/o/r.git", direct]);
    }

    #[test]
    fn fetch_candidates_does_not_duplicate_a_non_github_url() {
        assert_eq!(
            fetch_candidates("https://example.com/skill.md", true),
            ["https://example.com/skill.md"]
        );
    }
}
