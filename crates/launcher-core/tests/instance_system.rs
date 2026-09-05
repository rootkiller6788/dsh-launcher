//! Phase 2 acceptance tests: multi-instance CRUD, workspace isolation, and
//! SQLite launch-history persistence. Runs against a throwaway `AHL_HOME`.

use std::fs;
use std::path::PathBuf;

use launcher_core::{AppPaths, InstanceManifest, LaunchHistory};

fn tmp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ahl-p2-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn paths(home: &PathBuf) -> AppPaths {
    std::env::set_var("AHL_HOME", home);
    let p = AppPaths::from_env().expect("paths from env");
    p.ensure_dirs().expect("ensure dirs");
    p
}

#[test]
fn instance_crud_isolation_and_history() {
    let home = tmp_home("crud");
    let paths = paths(&home);

    // --- create: slug id from name, workspace materialized, isolated ---
    let coding = InstanceManifest::create(&paths, "Coding Agent").expect("create coding");
    assert_eq!(coding.id, "coding-agent");
    assert!(PathBuf::from(&coding.workspace).is_dir());

    // Fresh instance B's workspace starts empty; A owns its own dir.
    let research = InstanceManifest::create(&paths, "Research Agent").expect("create research");
    assert_eq!(research.id, "research-agent");
    assert!(PathBuf::from(&research.workspace).is_dir());
    assert_ne!(coding.workspace, research.workspace);

    // Isolation: a file written into A's workspace never appears in B's.
    fs::write(
        PathBuf::from(&coding.workspace).join("marker.txt"),
        "a-only",
    )
    .unwrap();
    assert!(!PathBuf::from(&research.workspace)
        .join("marker.txt")
        .exists());

    // --- list ---
    let all = InstanceManifest::list(&paths).expect("list");
    assert!(all.iter().any(|m| m.id == "coding-agent"));
    assert!(all.iter().any(|m| m.id == "research-agent"));

    // --- rename: id and workspace unchanged ---
    let renamed = InstanceManifest::rename(&paths, "coding-agent", "Coding Pro").expect("rename");
    assert_eq!(renamed.name, "Coding Pro");
    assert_eq!(renamed.id, "coding-agent");
    assert_eq!(renamed.workspace, coding.workspace);

    // --- clone: new id + deep-copied workspace ---
    let copy = InstanceManifest::clone(&paths, "coding-agent", "Research Agent").expect("clone");
    assert_eq!(copy.id, "research-agent-2");
    assert!(
        PathBuf::from(&copy.workspace).join("marker.txt").exists(),
        "clone deep-copies workspace"
    );
    // Editing the copy must not touch the source.
    fs::write(
        PathBuf::from(&copy.workspace).join("marker.txt"),
        "copy-only",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(PathBuf::from(&coding.workspace).join("marker.txt")).unwrap(),
        "a-only"
    );

    // --- delete: last instance is protected, others removable ---
    // At this point: coding-agent, research-agent, research-agent-2 (3 instances).
    InstanceManifest::delete(&paths, "research-agent-2").expect("delete clone");
    let remaining = InstanceManifest::list(&paths).expect("list after delete");
    assert_eq!(remaining.len(), 2);

    InstanceManifest::delete(&paths, "research-agent").expect("delete non-last instance");
    let remaining = InstanceManifest::list(&paths).expect("list final");
    assert_eq!(remaining.len(), 1);
    assert!(
        InstanceManifest::delete(&paths, "coding-agent").is_err(),
        "cannot delete the last instance"
    );
    assert_eq!(
        InstanceManifest::list(&paths)
            .expect("list after refused delete")
            .len(),
        1
    );

    // --- history: record, read back, close, persist across reopen ---
    let db = paths.db_file();
    let hist = LaunchHistory::open(&db).expect("open history");
    let sid = hist.record_start("coding-agent").expect("record start");
    let recent = hist.recent(10).expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].status, "running");
    assert_eq!(recent[0].instance_id, "coding-agent");

    hist.record_end(sid, "stopped", None).expect("record end");
    let recent = hist.recent(10).expect("recent after end");
    assert_eq!(recent[0].status, "stopped");
    assert!(recent[0].ended_at.is_some());

    // Re-open the DB → row survives (persistence).
    let hist2 = LaunchHistory::open(&db).expect("reopen history");
    let recent2 = hist2.recent(10).expect("recent after reopen");
    assert_eq!(recent2.len(), 1);
    assert_eq!(recent2[0].status, "stopped");
    assert_eq!(recent2[0].instance_id, "coding-agent");

    // --- cleanup ---
    let _ = fs::remove_dir_all(&home);
}
