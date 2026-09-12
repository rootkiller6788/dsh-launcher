//! Stable, user-visible error codes shared by every surface that reports a
//! failure: the popup banner, the Activity log, and (in Phase 2.6) the
//! redacted diagnostic package. Mirrors the idea behind `1/`'s `ErrorCodes.cs`
//! — one code, one category, one "next action", consumed by all three.
//!
//! The code is the *stable* identifier; the dynamic detail (a path, an exit
//! code, a server name) lives in [`CodedError::message`] and is supplied by the
//! call site. Categories follow `1/`'s bands:
//!
//! | band | meaning |
//! |------|---------|
//! | `E1xxx` | runtime — dsh / Node can't start or stay up |
//! | `E2xxx` | service — a user action (install/remove/update) failed |
//! | `E4xxx` | update — refresh / update-check failed |
//! | `E9xxx` | internal — unexpected, no better bucket |

use serde::{Deserialize, Serialize};

/// A single error code and its static metadata. The code string is a contract:
/// do not reuse a code for a different meaning, and do not change a code's
/// category once it ships (diagnostic tooling keys on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // — runtime (E1xxx) —
    /// Node.js is missing or not resolvable for launching dsh.
    NodeNotFound,
    /// The dsh binary could not be located (`resolve_bin`).
    DshBinUnresolvable,
    /// dsh started but failed before reaching a usable web server.
    LaunchFailed,
    /// dsh went silent past the adaptive silence timeout.
    BootTimedOut,
    /// The webview loaded dsh's token-rejected page instead of the UI.
    TokenInvalid,
    // — service (E2xxx) —
    /// `dsh plugin add/remove/update` exited non-zero.
    PluginOpFailed,
    /// Skill download / unpack failed.
    SkillInstallFailed,
    /// MCP install or probe failed.
    McpOpFailed,
    /// Managed runtime install / switch / probe failed.
    RuntimeOpFailed,
    /// The remote content catalog could not be fetched.
    MarketFetchFailed,
    // — update (E4xxx) —
    /// Refresh / update-check failed.
    UpdateCheckFailed,
    // — internal (E9xxx) —
    /// Untagged / unexpected failure.
    Internal,
}

impl ErrorCode {
    /// The stable machine-readable code, e.g. `E1001`.
    pub fn code(self) -> &'static str {
        match self {
            Self::NodeNotFound => "E1001",
            Self::DshBinUnresolvable => "E1002",
            Self::LaunchFailed => "E1003",
            Self::BootTimedOut => "E1004",
            Self::TokenInvalid => "E1005",
            Self::PluginOpFailed => "E2001",
            Self::SkillInstallFailed => "E2002",
            Self::McpOpFailed => "E2003",
            Self::RuntimeOpFailed => "E2004",
            Self::MarketFetchFailed => "E2005",
            Self::UpdateCheckFailed => "E4001",
            Self::Internal => "E9001",
        }
    }

    /// The band, one of `runtime` / `service` / `update` / `internal`.
    pub fn category(self) -> &'static str {
        match self {
            Self::NodeNotFound
            | Self::DshBinUnresolvable
            | Self::LaunchFailed
            | Self::BootTimedOut
            | Self::TokenInvalid => "runtime",
            Self::PluginOpFailed
            | Self::SkillInstallFailed
            | Self::McpOpFailed
            | Self::RuntimeOpFailed
            | Self::MarketFetchFailed => "service",
            Self::UpdateCheckFailed => "update",
            Self::Internal => "internal",
        }
    }

    /// Short human label for the failure itself (what happened).
    pub fn title(self) -> &'static str {
        match self {
            Self::NodeNotFound => "Node.js is missing",
            Self::DshBinUnresolvable => "DSH binary not found",
            Self::LaunchFailed => "DSH failed to start",
            Self::BootTimedOut => "DSH startup timed out",
            Self::TokenInvalid => "DSH token rejected",
            Self::PluginOpFailed => "Plugin change failed",
            Self::SkillInstallFailed => "Skill install failed",
            Self::McpOpFailed => "MCP server failed",
            Self::RuntimeOpFailed => "Runtime operation failed",
            Self::MarketFetchFailed => "Catalog fetch failed",
            Self::UpdateCheckFailed => "Update check failed",
            Self::Internal => "Unexpected error",
        }
    }

    /// The "next action" guidance shown to the user (what to do about it).
    pub fn next_action(self) -> &'static str {
        match self {
            Self::NodeNotFound => "Install Node.js, or add a managed runtime in Settings → Runtime.",
            Self::DshBinUnresolvable => "Reinstall DSH, or point DSH_CLI_BIN at the binary.",
            Self::LaunchFailed => "Check the Activity log for the dsh detail, then retry.",
            Self::BootTimedOut => "The instance may be heavy — give it more time, or check the Activity log.",
            Self::TokenInvalid => "Re-enter your API key in Settings, then relaunch.",
            Self::PluginOpFailed => "Check the package name and your network, then retry (detail in Activity).",
            Self::SkillInstallFailed => "Check the skill source resolves and your network can reach it.",
            Self::McpOpFailed => "Check the server config and that its runtime is installed.",
            Self::RuntimeOpFailed => "Check the runtime version and that the source tree is valid.",
            Self::MarketFetchFailed => "Check your network; the in-box snapshot is used meanwhile.",
            Self::UpdateCheckFailed => "Check your network, then retry the refresh.",
            Self::Internal => "Retry; if it persists, export a diagnostic package.",
        }
    }
}

/// A coded, user-visible failure: a stable [`ErrorCode`] plus the dynamic
/// detail. This is the unit that travels to every reporting surface, and it
/// serializes to the shape the frontend renders in the banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodedError {
    pub code: String,
    pub category: String,
    pub title: String,
    pub message: String,
    pub next_action: String,
}

impl CodedError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code().to_string(),
            category: code.category().to_string(),
            title: code.title().to_string(),
            message: message.into(),
            next_action: code.next_action().to_string(),
        }
    }

    /// Untagged fallback — surfaced as `E9001` internal.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// One-line form for the Activity log: `[E2001] message`.
    pub fn log_line(&self) -> String {
        format!("[{}] {}", self.code, self.message)
    }
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_categorised() {
        assert_eq!(ErrorCode::NodeNotFound.code(), "E1001");
        assert_eq!(ErrorCode::NodeNotFound.category(), "runtime");
        assert_eq!(ErrorCode::PluginOpFailed.code(), "E2001");
        assert_eq!(ErrorCode::PluginOpFailed.category(), "service");
        assert_eq!(ErrorCode::UpdateCheckFailed.code(), "E4001");
        assert_eq!(ErrorCode::UpdateCheckFailed.category(), "update");
        assert_eq!(ErrorCode::Internal.code(), "E9001");
        assert_eq!(ErrorCode::Internal.category(), "internal");
    }

    #[test]
    fn every_code_has_metadata() {
        // Cheap completeness guard: each band is covered by at least one code,
        // and none of the static strings are empty.
        for code in [
            ErrorCode::NodeNotFound,
            ErrorCode::DshBinUnresolvable,
            ErrorCode::LaunchFailed,
            ErrorCode::BootTimedOut,
            ErrorCode::TokenInvalid,
            ErrorCode::PluginOpFailed,
            ErrorCode::SkillInstallFailed,
            ErrorCode::McpOpFailed,
            ErrorCode::RuntimeOpFailed,
            ErrorCode::MarketFetchFailed,
            ErrorCode::UpdateCheckFailed,
            ErrorCode::Internal,
        ] {
            assert!(!code.code().is_empty());
            assert!(!code.title().is_empty());
            assert!(!code.next_action().is_empty());
        }
    }

    #[test]
    fn coded_error_serializes_camel_case() {
        let e = CodedError::new(ErrorCode::BootTimedOut, "no output for 120s");
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["code"], "E1004");
        assert_eq!(json["message"], "no output for 120s");
        assert_eq!(json["nextAction"], ErrorCode::BootTimedOut.next_action());
    }

    #[test]
    fn log_line_prefixes_the_code() {
        let e = CodedError::new(ErrorCode::PluginOpFailed, "npm registry unreachable");
        assert_eq!(e.log_line(), "[E2001] npm registry unreachable");
    }

    #[test]
    fn internal_fallback_is_e9001() {
        let e = CodedError::internal("boom");
        assert_eq!(e.code, "E9001");
        assert_eq!(e.category, "internal");
    }
}
