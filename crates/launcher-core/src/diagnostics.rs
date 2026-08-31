use serde::{Deserialize, Serialize};

/// One row of an environment check (node / git / …).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvItem {
    pub name: String,
    pub present: bool,
    pub version: Option<String>,
    pub note: Option<String>,
}

/// Run `<name> <version_flag>` and summarize. Generic so the core stays
/// harness-agnostic; DSH itself is checked by the adapter.
pub fn check_tool(name: &str, version_flag: &str) -> EnvItem {
    match std::process::Command::new(name).arg(version_flag).output() {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_start_matches('v')
                .to_string();
            EnvItem {
                name: name.into(),
                present: true,
                version: Some(version),
                note: None,
            }
        }
        _ => EnvItem {
            name: name.into(),
            present: false,
            version: None,
            note: Some(format!("`{name}` not found on PATH")),
        },
    }
}
