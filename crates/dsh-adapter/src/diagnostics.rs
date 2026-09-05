//! Profile diagnostics — a read-only introspection of an instance's DSH
//! profile (a port of dsh-market's `check.ts` core). It re-scans the profile
//! directory on every call, no cache, and reports what boot would actually
//! mount: the bundle stack (load order source), duplicate loader entry ids,
//! orphan patch targets, and load-order constraint violations + suggestion.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use launcher_core::InstanceManifest;
use serde::{Deserialize, Serialize};

use crate::{parse_id, parse_inserted_ids, remove_row_blocks, DshAdapter};

/// One bundle in the load-order stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleInfo {
    pub name: String,
    pub resolved: bool,
    /// Entry ids this bundle's patch inserts (the rows it mounts).
    pub entry_ids: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderViolation {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub profile: String,
    pub bundles: Vec<BundleInfo>,
    /// Loader entry ids mounted by more than one layer (a boot error).
    pub duplicates: Vec<String>,
    /// Patch ids (profile/home layer) that target no bundle entry.
    pub orphans: Vec<String>,
    pub order_violations: Vec<OrderViolation>,
    /// Bundle names in a constraint-satisfying order.
    pub suggested_order: Vec<String>,
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&read(path)).ok()
}

fn profile_manifest(profile_dir: &Path) -> Option<serde_json::Value> {
    read_json(&profile_dir.join("package.json"))
}

/// The bundle list, in boot order, from `dsh.profile.bundles`.
fn bundle_stack(profile_dir: &Path) -> Vec<String> {
    profile_manifest(profile_dir)
        .and_then(|v| v.pointer("/dsh/profile/bundles").cloned())
        .and_then(|b| serde_json::from_value::<Vec<String>>(b).ok())
        .unwrap_or_default()
}

/// Locate a bundle's install directory: the profile's own `node_modules`
/// (out-of-tree plugins), then the launcher-maintained flat fallback
/// (`$DSH_HOME/profiles/node_modules`, one symlink per in-box package).
fn resolve_bundle_dir(profile_dir: &Path, workspace: &Path, name: &str) -> Option<PathBuf> {
    let candidates = [
        profile_dir.join("node_modules").join(name),
        workspace.join("profiles").join("node_modules").join(name),
    ];
    candidates
        .into_iter()
        .find(|d| d.join("package.json").is_file())
}

/// The entry ids a bundle contributes: the ids nested under `insert:` in its
/// declared `dsh.bundle.patch` and its conventional root `cordis.patch.yml`.
fn bundle_entry_ids(dir: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let declared = read_json(&dir.join("package.json"))
        .and_then(|v| v.pointer("/dsh/bundle/patch").and_then(|p| p.as_str()).map(String::from));
    if let Some(rel) = declared {
        ids.extend(parse_inserted_ids(&read(&dir.join(rel))));
    }
    ids.extend(parse_inserted_ids(&read(&dir.join("cordis.patch.yml"))));
    ids.sort();
    ids.dedup();
    ids
}

/// Top-level `- id:` entries of a patch layer (the rows it targets).
fn top_level_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            if let Some(id) = parse_id(trimmed) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// `dsh.bundle.order.{before,after}` constraint lists of a bundle.
fn bundle_order(dir: &Path) -> (Vec<String>, Vec<String>) {
    let Some(v) = read_json(&dir.join("package.json")) else {
        return (Vec::new(), Vec::new());
    };
    let list = |key: &str| -> Vec<String> {
        v.pointer(&format!("/dsh/bundle/order/{key}"))
            .and_then(|b| serde_json::from_value::<Vec<String>>(b.clone()).ok())
            .unwrap_or_default()
    };
    (list("before"), list("after"))
}

/// `(before, after)` map for every bundle in the stack, keyed by name.
fn order_constraints(
    bundle_names: &[String],
    profile_dir: &Path,
    workspace: &Path,
) -> HashMap<String, (Vec<String>, Vec<String>)> {
    let mut map = HashMap::new();
    for name in bundle_names {
        if let Some(dir) = resolve_bundle_dir(profile_dir, workspace, name) {
            map.insert(name.clone(), bundle_order(&dir));
        }
    }
    map
}

/// Validate the current order against before/after constraints.
fn order_violations(
    bundle_names: &[String],
    constraints: &HashMap<String, (Vec<String>, Vec<String>)>,
) -> Vec<OrderViolation> {
    let idx: HashMap<&String, usize> = bundle_names.iter().enumerate().map(|(i, n)| (n, i)).collect();
    let mut out = Vec::new();
    for (name, (before, after)) in constraints {
        for b in before {
            if let (Some(&i), Some(&j)) = (idx.get(name), idx.get(b)) {
                if i >= j {
                    out.push(OrderViolation {
                        name: name.clone(),
                        message: format!("must load before '{b}', but loads after it"),
                    });
                }
            }
        }
        for a in after {
            if let (Some(&i), Some(&j)) = (idx.get(name), idx.get(a)) {
                if i <= j {
                    out.push(OrderViolation {
                        name: name.clone(),
                        message: format!("must load after '{a}', but loads before it"),
                    });
                }
            }
        }
    }
    out
}

/// A minimal-move ordering that satisfies every before/after constraint,
/// keeping unconstrained bundles in their original relative order (stable
/// Kahn). Cycles fall back to the original order for the unresolved tail.
fn suggest_order(
    bundle_names: &[String],
    constraints: &HashMap<String, (Vec<String>, Vec<String>)>,
) -> Vec<String> {
    let known: HashSet<&String> = bundle_names.iter().collect();
    // edges: (from, to) means `from` loads before `to`.
    let mut edges: Vec<(String, String)> = Vec::new();
    for (name, (before, after)) in constraints {
        for b in before {
            if known.contains(b) {
                edges.push((name.clone(), b.clone()));
            }
        }
        for a in after {
            if known.contains(a) {
                edges.push((a.clone(), name.clone()));
            }
        }
    }

    let mut remaining: Vec<String> = bundle_names.to_vec();
    let mut out = Vec::new();
    loop {
        let mut idx = 0;
        let mut advanced = false;
        while idx < remaining.len() {
            let n = &remaining[idx];
            let blocked = edges.iter().any(|(f, t)| t == n && remaining.contains(f));
            if blocked {
                idx += 1;
            } else {
                out.push(remaining.remove(idx));
                advanced = true;
            }
        }
        if !advanced {
            break;
        }
    }
    // Unresolvable (cycle) — keep the original relative order for the rest.
    out.extend(remaining);
    out
}

/// Produce the diagnostics report for an instance's profile.
pub fn diagnose_profile(instance: &InstanceManifest) -> DiagnosticsReport {
    let profile_dir = DshAdapter::profile_dir(instance);
    let workspace = PathBuf::from(&instance.workspace);
    let bundle_names = bundle_stack(&profile_dir);

    let mut bundles = Vec::new();
    let mut entry_count: HashMap<String, usize> = HashMap::new();
    let mut all_ids = Vec::new();
    for name in &bundle_names {
        let (resolved, entry_ids, error) = match resolve_bundle_dir(&profile_dir, &workspace, name) {
            Some(dir) => (true, bundle_entry_ids(&dir), None),
            None => (
                false,
                Vec::new(),
                Some("bundle directory not found".to_string()),
            ),
        };
        for id in &entry_ids {
            *entry_count.entry(id.clone()).or_insert(0) += 1;
            all_ids.push(id.clone());
        }
        bundles.push(BundleInfo {
            name: name.clone(),
            resolved,
            entry_ids,
            error,
        });
    }

    let duplicates = entry_count
        .iter()
        .filter(|(_, c)| **c > 1)
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();

    let known: HashSet<String> = all_ids.iter().cloned().collect();
    let mut orphans = Vec::new();
    for patch_path in [
        profile_dir.join("cordis.patch.yml"),
        workspace.join("cordis.patch.yml"),
    ] {
        for id in top_level_ids(&read(&patch_path)) {
            if !known.contains(&id) && !orphans.contains(&id) {
                orphans.push(id);
            }
        }
    }

    let constraints = order_constraints(&bundle_names, &profile_dir, &workspace);
    let violations = order_violations(&bundle_names, &constraints);
    let suggested_order = suggest_order(&bundle_names, &constraints);

    DiagnosticsReport {
        profile: instance.profile.clone(),
        bundles,
        duplicates,
        orphans,
        order_violations: violations,
        suggested_order,
    }
}

/// Remove stale user patch rows that target no mounted bundle entry.
///
/// This intentionally only removes the simple enable/disable rows written by
/// the launcher (`- id: ...` + `disabled: true|false`). Other top-level patch
/// shapes stay untouched and will still be reported by diagnostics.
pub fn repair_orphan_toggle_rows(instance: &InstanceManifest) -> anyhow::Result<Vec<String>> {
    let report = diagnose_profile(instance);
    if report.orphans.is_empty() {
        return Ok(Vec::new());
    }

    let profile_dir = DshAdapter::profile_dir(instance);
    let workspace = PathBuf::from(&instance.workspace);
    let mut repaired = Vec::new();
    for patch_path in [
        profile_dir.join("cordis.patch.yml"),
        workspace.join("cordis.patch.yml"),
    ] {
        remove_row_blocks(&patch_path, &report.orphans)?;
        for id in &report.orphans {
            if !repaired.contains(id) {
                repaired.push(id.clone());
            }
        }
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn suggest_order_respects_constraints() {
        let ns = names(&["a", "b", "c"]);
        let mut constraints = HashMap::new();
        constraints.insert("b".to_string(), (vec![], vec!["a".to_string()]));
        let order = suggest_order(&ns, &constraints);
        let pa = order.iter().position(|x| x == "a").unwrap();
        let pb = order.iter().position(|x| x == "b").unwrap();
        assert!(pa < pb, "a must come before b: {order:?}");
        // c (unconstrained) keeps its relative spot.
        assert!(order.contains(&"c".to_string()));
    }

    #[test]
    fn order_violations_detects_bad_order() {
        let ns = names(&["b", "a"]);
        let mut constraints = HashMap::new();
        constraints.insert("b".to_string(), (vec![], vec!["a".to_string()]));
        let v = order_violations(&ns, &constraints);
        assert!(!v.is_empty());
    }

    #[test]
    fn repair_orphan_toggle_rows_removes_stale_disable_blocks() {
        let root = std::env::temp_dir().join(format!("dsh-diag-repair-{}", std::process::id()));
        let workspace = root.join("workspace");
        let profile = workspace.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dsh":{"profile":{"bundles":[]}}}"#,
        )
        .unwrap();
        std::fs::write(
            profile.join("cordis.patch.yml"),
            "- id: smart-market\n  disabled: true\n- id: plugin-market\n  disabled: false\n",
        )
        .unwrap();

        let instance = InstanceManifest {
            id: "repair".into(),
            name: "Repair".into(),
            runtime: launcher_core::RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "default".into(),
            plugins: vec![],
            skills: vec![],
            mcp: vec![],
            skins: vec![],
            workspace: workspace.display().to_string(),
        };

        let before = diagnose_profile(&instance);
        assert_eq!(before.orphans, vec!["smart-market", "plugin-market"]);

        let repaired = repair_orphan_toggle_rows(&instance).unwrap();
        assert_eq!(repaired, vec!["smart-market", "plugin-market"]);

        let after = diagnose_profile(&instance);
        assert!(after.orphans.is_empty());
        assert!(std::fs::read_to_string(profile.join("cordis.patch.yml"))
            .unwrap()
            .contains("[]"));

        let _ = std::fs::remove_dir_all(root);
    }
}
