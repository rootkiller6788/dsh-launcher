//! Page-level boot self-check for dsh's web UI.
//!
//! After the webview loads dsh's ready URL, the launcher probes the rendered
//! DOM for dsh's boot markers and classifies the result. Ported from `1/`'s
//! ADR-023 `BootSignature` (`ShellLogic.BootProfile` + `EvaluatePageProbe`),
//! adapted to Rust. The probe classifies **one** sample — the caller owns the
//! grace period, the interval, and the absent threshold.
//!
//! Why this exists: a token that dsh rejects, or a plugin whose module fails to
//! import, can still print a perfectly valid ready line. The process "started",
//! but the page is an error screen. Matching the page is the only signal that
//! distinguishes "DSH is up" from "DSH printed a URL then died on the page".

/// The signature table dsh's UI is matched against.
#[derive(Debug, Clone)]
pub struct PageSignature {
    /// JS expression that evaluates true once dsh's UI has booted. Covers both
    /// bootstrap generations: the old `window.__DSH_BOOT__.version` and the new
    /// `__ModuleLoader__.mode === "live"`.
    good_symbol: String,
    /// DOM / error text that marks the page as a fatal boot error.
    bad_signatures: Vec<String>,
    /// Matched **before** `good_symbol`: a fatal panel that replaces the UI but
    /// still carries the boot marker (a plugin module failed to import). One
    /// hit = failed.
    fatal_panel_signatures: Vec<String>,
    /// `body.innerText` length at or above which a signature-free page counts as
    /// "rendered" — dsh's own welcome/config UI (no key set) is healthy.
    rendered_min_text_chars: usize,
}

impl Default for PageSignature {
    fn default() -> Self {
        Self {
            good_symbol: "(window.__DSH_BOOT__&&window.__DSH_BOOT__.version)||(window.__ModuleLoader__&&window.__ModuleLoader__.mode===\"live\")"
                .to_string(),
            bad_signatures: vec![
                "bootstrap facade is missing".into(),
                "plugin fatal".into(),
                "dsh-boot-failed".into(),
            ],
            fatal_panel_signatures: vec!["failed to import loader entry".into()],
            rendered_min_text_chars: 60,
        }
    }
}

/// One classified probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeClass {
    /// `good_symbol` evaluated true — healthy, stop probing.
    GoodSymbol,
    /// A fatal marker was hit (one-hit kill). `detail` carries the excerpt.
    BadSignature { detail: String },
    /// Substantial content rendered with no bad signature — healthy-equivalent.
    Rendered,
    /// Good symbol absent (counted toward an absent threshold by the caller).
    Absent { detail: Option<String> },
    /// Null / empty / unparseable probe result — the probe itself failed, and
    /// must never be treated as a boot failure.
    Invalid,
}

impl PageSignature {
    /// The probe script to run in the page. Returns JSON `{good,text,err}`; the
    /// script itself is wrapped in try/catch so it never throws into the page.
    pub fn build_probe_script(&self) -> String {
        format!(
            "(function(){{try{{var t=(document.body&&document.body.innerText)?document.body.innerText.slice(0,2000):'';\
             var e=(window.__dshLastError&&window.__dshLastError.message)||'';\
             return JSON.stringify({{good:!!({}),text:t,err:e}});}}catch(x)\
             {{return JSON.stringify({{good:false,text:'',err:'probe-error:'+x.message}});}})()",
            self.good_symbol
        )
    }

    /// Classify one probe result. Ports `1/`'s `EvaluatePageProbe` order exactly:
    /// fatal panel → error-text bad signature → good symbol → dom bad signature
    /// (degraded to Absent) → rendered → absent.
    pub fn evaluate(&self, script_json: &str) -> ProbeClass {
        let value = match parse_probe(script_json) {
            Some(v) => v,
            None => return ProbeClass::Invalid,
        };
        let good = value["good"].as_bool() == Some(true);
        let text = value["text"].as_str().unwrap_or("");
        let err = value["err"].as_str().unwrap_or("");

        // Fatal panel first: it can coexist with a healthy-looking boot marker.
        if let Some(hit) = match_bad_signature(text, &self.fatal_panel_signatures) {
            return ProbeClass::BadSignature {
                detail: format!("dom[{hit}]={}", truncate(text, 300)),
            };
        }
        // Error text bad signature is a one-hit kill, and carries the exception.
        if !err.is_empty() {
            if let Some(hit) = match_bad_signature(err, &self.bad_signatures) {
                return ProbeClass::BadSignature {
                    detail: format!("err[{hit}]={}", truncate(err, 300)),
                };
            }
        }
        // Good symbol: healthy, and exempts hidden bad-signature literals that a
        // real UI might legitimately contain.
        if good {
            return ProbeClass::GoodSymbol;
        }
        // DOM bad signature degrades to Absent (not a one-hit kill) — the caller
        // confirms over the absent threshold, keeping single samples from killing
        // an otherwise fine boot.
        if let Some(hit) = match_bad_signature(text, &self.bad_signatures) {
            return ProbeClass::Absent {
                detail: Some(format!("dom-suspect[{hit}]={}", truncate(text, 300))),
            };
        }
        // Rendered exemption: dsh's own welcome/config UI is healthy.
        if text.chars().count() >= self.rendered_min_text_chars {
            return ProbeClass::Rendered;
        }
        ProbeClass::Absent {
            detail: (!err.is_empty()).then(|| format!("err={}", truncate(err, 200))),
        }
    }
}

/// Parse a probe result, unwrapping the double-encoded string shape that a
/// webview `eval` of `return JSON.stringify(…)` can produce.
fn parse_probe(script_json: &str) -> Option<serde_json::Value> {
    if script_json.trim().is_empty() || script_json.trim() == "undefined" {
        return None;
    }
    let mut value: serde_json::Value = serde_json::from_str(script_json).ok()?;
    // The probe returns a string literal, so a webview eval that JSON-encodes its
    // result wraps it once more — unwrap one layer.
    if let Some(inner) = value.as_str() {
        value = serde_json::from_str(inner).ok()?;
    }
    (value.is_object()).then_some(value)
}

/// Case-insensitive substring match against a signature list; returns the hit.
fn match_bad_signature<'a>(text: &str, signatures: &'a [String]) -> Option<&'a str> {
    let lower = text.to_ascii_lowercase();
    signatures
        .iter()
        .find(|s| lower.contains(&s.to_ascii_lowercase()))
        .map(String::as_str)
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(good: bool, text: &str, err: &str) -> String {
        format!(
            "{{\"good\":{},\"text\":{},\"err\":{}}}",
            good,
            serde_json::to_string(text).unwrap(),
            serde_json::to_string(err).unwrap()
        )
    }

    #[test]
    fn good_symbol_is_healthy() {
        let sig = PageSignature::default();
        assert_eq!(
            sig.evaluate(&probe(true, "welcome to dsh", "")),
            ProbeClass::GoodSymbol
        );
    }

    #[test]
    fn fatal_panel_wins_over_good_symbol() {
        // A plugin fatal panel can render on a page that still sets the boot
        // marker — the panel is the deterministic kill, not the marker.
        let sig = PageSignature::default();
        let out = sig.evaluate(&probe(true, "Failed to import loader entry foo", ""));
        match out {
            ProbeClass::BadSignature { detail } => {
                assert!(detail.starts_with("dom[failed to import loader entry]"), "{detail}");
            }
            other => panic!("expected BadSignature, got {other:?}"),
        }
    }

    #[test]
    fn error_text_bad_signature_is_a_one_hit_kill() {
        let sig = PageSignature::default();
        let out = sig.evaluate(&probe(false, "", "plugin fatal: module missing"));
        match out {
            ProbeClass::BadSignature { detail } => {
                assert!(detail.starts_with("err[plugin fatal]"), "{detail}");
            }
            other => panic!("expected BadSignature, got {other:?}"),
        }
    }

    #[test]
    fn dom_bad_signature_degrades_to_absent() {
        let sig = PageSignature::default();
        let out = sig.evaluate(&probe(false, "dsh-boot-failed", ""));
        assert!(matches!(out, ProbeClass::Absent { .. }), "got {out:?}");
    }

    #[test]
    fn rendered_content_is_healthy() {
        let sig = PageSignature::default();
        let text = "configure your API key to get started with deepseek harness here";
        assert_eq!(sig.evaluate(&probe(false, text, "")), ProbeClass::Rendered);
    }

    #[test]
    fn blank_page_is_absent() {
        let sig = PageSignature::default();
        assert!(matches!(
            sig.evaluate(&probe(false, "", "")),
            ProbeClass::Absent { .. }
        ));
    }

    #[test]
    fn unparseable_result_is_invalid_not_failed() {
        let sig = PageSignature::default();
        assert_eq!(sig.evaluate("undefined"), ProbeClass::Invalid);
        assert_eq!(sig.evaluate(""), ProbeClass::Invalid);
        assert_eq!(sig.evaluate("not json"), ProbeClass::Invalid);
    }

    #[test]
    fn double_encoded_result_is_unwrapped() {
        let sig = PageSignature::default();
        // A webview eval may wrap the probe's JSON string in another JSON string.
        let inner = probe(true, "hi", "");
        let double = serde_json::to_string(&inner).unwrap();
        assert_eq!(sig.evaluate(&double), ProbeClass::GoodSymbol);
    }

    #[test]
    fn probe_script_embeds_the_good_symbol() {
        let sig = PageSignature::default();
        let script = sig.build_probe_script();
        assert!(script.contains("__DSH_BOOT__"), "{script}");
        assert!(script.contains("__ModuleLoader__"), "{script}");
        assert!(script.contains("__dshLastError"), "{script}");
    }
}
