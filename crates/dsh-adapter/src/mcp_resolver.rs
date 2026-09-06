// Runtime MCP resolver fallback (roadmap §8.3 / Phase 1, decision 1).
//
// The dev-time batch (`scripts/resolver/github-analyzer.mjs`) precomputes
// `mcpInstall` for the catalog; this module is the *single-entry runtime fallback*
// for entries that ship WITHOUT a precomputed plan and have no launch command at
// all (fresh/hosted-snapshot entries). It does a quick github probe:
//
//   package.json (node) → npm published name → `npx -y <name>`
//   pyproject.toml / setup.py (python) → PyPI published name → `uvx <name>`
//
// Dockerfile decompile stays a dev-time concern (it needs a repo-wide decision
// the batch runs once); the runtime path only claims the high-confidence case —
// a repo whose root manifest names a *published* package. Anything else returns
// `None` and the entry keeps its current behavior (open GitHub / pseudo command).

use std::time::Duration;

use launcher_core::market::npm_registry;
use launcher_core::{McpInstallManifest, McpLaunchSpec, RegistryPlugin};

const PYPI_MIRROR: &str = "https://pypi.org/pypi";

/// Probe one github repo and, when its root manifest names a package that is
/// actually published, return a canonical install plan. `hint` is the runtime
/// suggested by an existing command (`npx`/`uvx`), used to order the probes.
pub async fn probe_mcp_install(entry: &RegistryPlugin, hint: Option<&str>) -> Option<McpInstallManifest> {
    let (owner, repo) = owner_repo(entry.url.as_str())?;
    let client = http_client();

    // Probe order: package.json for node-ish hints; pyproject/setup.py for python.
    let order: Vec<(&str, &str)> = match hint {
        Some("npx") => vec![("package.json", "package")],
        Some("uvx") => vec![("pyproject.toml", "pyproject"), ("setup.py", "setup")],
        _ => vec![
            ("package.json", "package"),
            ("pyproject.toml", "pyproject"),
            ("setup.py", "setup"),
        ],
    };

    let mut pkg_name: Option<String> = None;
    let mut py_name: Option<String> = None;
    for (file, kind) in order {
        let Ok(text) = fetch_raw(&client, &owner, &repo, file).await else {
            continue;
        };
        match kind {
            "package" => pkg_name = package_name(&text),
            "pyproject" => py_name = pyproject_name(&text),
            _ => {
                if py_name.is_none() {
                    py_name = setup_name(&text);
                }
            }
        }
    }

    if let Some(name) = pkg_name {
        if !name.contains("github:") && published_npm(&client, &name).await {
            return Some(node_plan(&name));
        }
    }
    if let Some(name) = py_name {
        if published_pypi(&client, &name).await {
            return Some(python_plan(&name));
        }
    }
    None
}

fn node_plan(name: &str) -> McpInstallManifest {
    McpInstallManifest {
        runtime: "node".into(),
        method: "npm".into(),
        package: name.to_string(),
        launch: McpLaunchSpec { command: "npx".into(), args: vec!["-y".into(), name.into()] },
    }
}

fn python_plan(name: &str) -> McpInstallManifest {
    McpInstallManifest {
        runtime: "python".into(),
        method: "uv".into(),
        package: name.to_string(),
        launch: McpLaunchSpec { command: "uvx".into(), args: vec![name.into()] },
    }
}

// --- parsers (pure) ---------------------------------------------------------

fn owner_repo(url: &str) -> Option<(String, String)> {
    let rest = url.trim().trim_end_matches('/').strip_prefix("https://github.com/")?;
    let mut segs = rest.split('/');
    let owner = segs.next()?.to_string();
    let repo = segs.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

fn package_name(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let name = v.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() || v.get("private").and_then(|p| p.as_bool()).unwrap_or(false) {
        return None;
    }
    Some(name)
}

fn pyproject_name(text: &str) -> Option<String> {
    let mut in_project = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_project = line.trim_start_matches('[').trim_end_matches(']').trim() == "project";
            continue;
        }
        if in_project {
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim();
            if key == "name" {
                let val = line[eq + 1..].trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn setup_name(text: &str) -> Option<String> {
    // `name="pkg"` / `name = 'pkg'` in a setup() call — simple scan, no regex dep.
    let mut rest = text;
    while let Some(needle) = rest.find("name") {
        let after = &rest[needle + 4..];
        let after = after.trim_start_matches([' ', '\t', '\n']);
        let after = after.strip_prefix('=')?;
        let after = after.trim_start_matches([' ', '\t', '\n']);
        let quote = after.chars().next()?;
        if quote == '"' || quote == '\'' {
            let end = after[1..].find(quote)?;
            let val = after[1..=end].trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
        rest = after; // keep scanning later occurrences of "name"
    }
    None
}

// --- network ----------------------------------------------------------------

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

async fn fetch_raw(client: &reqwest::Client, owner: &str, repo: &str, file: &str) -> Result<String, anyhow::Error> {
    let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{file}");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    let text = resp.text().await?;
    if text.trim().is_empty() {
        anyhow::bail!("empty body");
    }
    Ok(text)
}

async fn published_npm(client: &reqwest::Client, name: &str) -> bool {
    let url = format!("{}/{}", npm_registry().trim_end_matches('/'), urlencoding(name));
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

async fn published_pypi(client: &reqwest::Client, name: &str) -> bool {
    let url = format!("{PYPI_MIRROR}/{}/json", name);
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

fn urlencoding(name: &str) -> String {
    // Scoped npm packages need the `/` percent-encoded: `@scope/pkg` → `@scope%2Fpkg`.
    name.replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_repo_parses() {
        assert_eq!(
            owner_repo("https://github.com/acme/server").unwrap(),
            ("acme".into(), "server".into())
        );
        assert_eq!(
            owner_repo("https://github.com/acme/server.git").unwrap(),
            ("acme".into(), "server".into())
        );
        assert_eq!(owner_repo("https://example.com/x"), None);
    }

    #[test]
    fn package_name_skips_private() {
        assert_eq!(
            package_name(r#"{ "name": "@acme/mcp", "version": "1.0.0" }"#).unwrap(),
            "@acme/mcp"
        );
        assert_eq!(package_name(r#"{ "name": "x", "private": true }"#), None);
        assert_eq!(package_name("not json"), None);
    }

    #[test]
    fn pyproject_name_parses() {
        assert_eq!(
            pyproject_name("[project]\nname = \"mcp-git\"\nversion=\"1\"").unwrap(),
            "mcp-git"
        );
        assert_eq!(pyproject_name("[build-system]\nrequires=[]\n"), None);
    }

    #[test]
    fn setup_name_parses() {
        assert_eq!(setup_name("setup(name=\"mcp-git\", version=\"0.1\")").unwrap(), "mcp-git");
        assert_eq!(setup_name("x = 1"), None);
    }

    #[test]
    fn plan_shapes() {
        let p = node_plan("@acme/mcp");
        assert_eq!(p.runtime, "node");
        assert_eq!(p.launch.command, "npx");
        assert_eq!(p.launch.args, vec!["-y", "@acme/mcp"]);
        let p = python_plan("mcp-git");
        assert_eq!(p.method, "uv");
        assert_eq!(p.launch.args, vec!["mcp-git"]);
    }
}
