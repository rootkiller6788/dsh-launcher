// Acceptance walk — the launcher's whole job, in the order a user does it, on a
// throwaway data root.
//
//   cargo test -p dsh-adapter --test acceptance_e2e -- --ignored --nocapture
//
// Seven steps, one process, one root, in the order the GUI runs them:
//
//   1  create an instance       InstanceManifest::create          → instance.json + workspace
//   2  install the four kinds   content::install_skill            (github — the one networked step)
//                               mcp_local::install_local + probe  (local git fixture)
//                               sync_skin_patch                   → the skin's insert row
//                               the profile deps + package        → installed_plugins sees it
//   3  Library sees it          every projection re-read from disk, cold
//   4  launch it                spawn_child → logs → ready → stop, under the PID ledger
//   5  usage lands              UsageLedger::record / summary, per instance
//   6  export the environment   EnvironmentManifest + its checksum
//   7  import it                a fresh instance, every leaf installed again
//
// Steps run in order and share the root, so the printed trace *is* the
// acceptance report and a failure names the step it stopped at. The root is
// removed on success and left behind (with its path printed) on failure.
//
// Boundary: where a step's work belongs to DSH rather than the launcher, the
// walk asserts the artifact the launcher hands over and the runtime contract it
// depends on, instead of re-testing DSH. Specifically — `dsh plugin add`
// running pnpm for the skin/plugin rows, and the harness actually booting and
// serving its URL, need the vendored runtime and a live window; the click-
// through that covers those is the tauri-driver walk, not this one. What the
// launch step drives here is the launcher's own half of a launch (spawn → log
// capture → readiness → kill-tree → ledger row), which is the half the launcher
// is answerable for.
//
// Network: `git` and `node` are hard requirements (the launcher cannot run
// without either), the MCP row installs from a *local* git fixture, and only
// the skill row reaches github. Set `AHL_WALK_MIRROR=1` to route that one clone
// through the gh-proxy relay, the way the Install Center's mirror toggle does.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dsh_adapter::content::{install_skill, sync_skin_patch};
use dsh_adapter::mcp_local::install_local;
use dsh_adapter::mcp_probe::probe_mcp;
use dsh_adapter::DshAdapter;
use launcher_core::environment::{ENVIRONMENT_FORMAT, ENVIRONMENT_FORMAT_VERSION};
use launcher_core::market::{self, ContentKind};
use launcher_core::process::{
    pid_alive, spawn_child_with_exit, sweep_leftover, wait_for_port, ExitSink, PidLedger,
    ProcessStatus,
};
use launcher_core::{
    file_sha256, sha256_hex, AppPaths, EnvironmentManifest, EnvironmentSource, ExportedItem,
    InstanceManifest, LogLine, LogSink, McpServerRecord, NewUsageRecord, RegistryPlugin,
    UsageLedger, MCP_STATE_OK,
};

/// The catalog skill the walk installs. It has to be a *real* bundled entry:
/// the point of the step is that a skill the Market actually ships still
/// installs. `rootkiller6788/mathmodel-skill` is single-skill and tiny, so the
/// walk's two installs of it are fast.
const SKILL_KEY: &str = "rootkiller6788/mathmodel-skill";

/// The three fixture entries the walk installs alongside the skill. Their npm
/// names are what `dsh plugin add` would install; the walk materializes what
/// that leaves on disk (see [`install_leaf`]).
const PKG_PLUGIN: &str = "@acceptance/toolbox";
const PKG_SKIN: &str = "dsh-skin-acceptance";
const MCP_SERVER: &str = "walk-srv";

/// A minimal stdio MCP server: answers `initialize` + `tools/list`, so the
/// probe's handshake is a real one rather than a stub of the probe.
const MCP_SRV: &str = r#"const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
function send(o) { process.stdout.write(JSON.stringify(o) + '\n'); }
rl.on('line', (line) => {
  let m; try { m = JSON.parse(line); } catch { return; }
  if (m.method === 'initialize') {
    send({ jsonrpc: '2.0', id: m.id, result: {
      protocolVersion: (m.params && m.params.protocolVersion) || '2025-03-26',
      capabilities: { tools: {} },
      serverInfo: { name: 'walk-srv', version: '1.0.0' } } });
  } else if (m.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: m.id, result: { tools: [
      { name: 'ping', description: 'p', inputSchema: { type: 'object', properties: {} } } ] } });
  } else if (m.id !== undefined) {
    send({ jsonrpc: '2.0', id: m.id, result: {} });
  }
});
"#;

/// The launch step's stand-in child: an HTTP server that reports its own port
/// on stdout. That mirrors how a launch learns readiness — the launcher reads
/// DSH's URL line off the child's output, and this reads the port off this
/// one.
const HTTP_STANDIN: &str = r#"const http = require('http');
const srv = http.createServer((_q, res) => res.end('ok'));
srv.listen(0, '127.0.0.1', () => console.log('listening ' + srv.address().port));
"#;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The walk's log sink: prints each line and keeps it, so the launch step can
/// read readiness out of the child's own output.
#[derive(Clone)]
struct Logs(Arc<Mutex<Vec<String>>>);

impl Logs {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn sink(&self) -> LogSink {
        let store = self.0.clone();
        Arc::new(move |line: LogLine| {
            eprintln!("      │ {}", line.line);
            if let Ok(mut all) = store.lock() {
                all.push(line.line);
            }
        })
    }

    /// Wait until a captured line satisfies `pred`, returning it.
    fn wait_line(&self, timeout: Duration, pred: impl Fn(&str) -> bool) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(all) = self.0.lock() {
                if let Some(found) = all.iter().find(|l| pred(l)) {
                    return Some(found.clone());
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn step(n: u8, what: &str) {
    eprintln!("\n[{n}/7] {what}");
}

fn ok(what: impl std::fmt::Display) {
    eprintln!("      ✓ {what}");
}

/// Poll `pred` until it holds or `timeout` elapses. Used where the OS decides
/// when something is true (a killed process is gone), never where a fixed
/// sleep would do.
fn eventually(timeout: Duration, pred: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn workdir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ahl-acceptance-{tag}-{}-{:x}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default()
    ))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// Build a real local git repo so the MCP row exercises the same
/// shallow-clone-then-build path a github source-run takes.
fn make_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    for (rel, body) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "walk@test.local"],
        vec!["config", "user.name", "walk"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(&args)
            .output()
            .expect("git runnable");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

// ---------------------------------------------------------------------------
// The one step the Market and the importer share
// ---------------------------------------------------------------------------

/// Install one catalog item into `instance` and persist it — the same four
/// installs the Market runs, keyed by kind.
///
/// This is shared by step 2 and step 7 on purpose: an environment import *is*
/// these installs against a different instance, so one function making both
/// calls is the claim worth testing. Reused by the MCP fixture install into a
/// fresh clone directory per call (an import re-resolves its leaves rather than
/// pointing at the exporting instance's files).
fn install_leaf(
    paths: &AppPaths,
    instance: &InstanceManifest,
    item: &RegistryPlugin,
    mirror: bool,
    logs: &Logs,
) -> InstanceManifest {
    let key = item.key();
    match item.kind {
        ContentKind::Skill => {
            let record = rt()
                .block_on(install_skill(instance, item, mirror))
                .unwrap_or_else(|e| panic!("install skill {key}: {e}"));
            ok(format!("skill {key} → skills/{} (sha256 {})", record.id.replace('/', "-"), &record.hash[..12]));
            InstanceManifest::add_skill(paths, &instance.id, &record).expect("persist the skill")
        }
        ContentKind::Mcp => {
            let record = McpServerRecord {
                id: key.clone(),
                server_name: item.name.clone(),
                transport: item.transport.clone().unwrap_or_else(|| "stdio".into()),
                command: item.command.clone().unwrap_or_default(),
                args: item.args.clone().unwrap_or_default(),
                env: item.env.clone().unwrap_or_default(),
                url: item.mcp_url.clone().unwrap_or_default(),
                ..McpServerRecord::default()
            };
            // The health snapshot the Library's MCP badge reads: persisted next
            // to the manifest, keyed by the sanitized server name.
            let state = rt().block_on(probe_mcp(&record, None, logs.sink()));
            assert_eq!(state.state, MCP_STATE_OK, "MCP {key} must handshake, err: {:?}", state.error);
            assert!(!state.tools.is_empty(), "MCP {key} answered no tools/list");
            launcher_core::save_runtime(
                &paths.mcp_runtime_file(&instance.id, &record.server_name),
                &state,
            )
            .expect("persist the MCP health snapshot");
            ok(format!("mcp {key} → {} tool(s), state={}", state.tools.len(), state.state));
            InstanceManifest::add_mcp(paths, &instance.id, &record).expect("persist the MCP")
        }
        ContentKind::Theme | ContentKind::Plugin => {
            let package = item.install_spec();
            assert!(!package.is_empty(), "{key} has no npm package to install");
            materialize_profile_package(instance, &package, item.kind == ContentKind::Theme);
            if item.kind == ContentKind::Theme {
                // install ≠ enable: a fresh skin lands disabled, and mounting it
                // is the explicit toggle that compiles the patch insert.
                InstanceManifest::add_skin_package(paths, &instance.id, &key, &package)
                    .expect("persist the skin");
                let enabled = InstanceManifest::set_skin_enabled(paths, &instance.id, &key, true)
                    .expect("enable the skin");
                sync_skin_patch(&enabled, &enabled.skin_packages).expect("compile the skin patch");
                ok(format!("skin {key} → {package} (insert row compiled)"));
            } else {
                ok(format!("plugin {key} → {package} (profile dep + patch rows)"));
            }
            InstanceManifest::get(paths, &instance.id).expect("re-read the instance")
        }
        ContentKind::Bundle => {
            panic!("{key} is a bundle — the walk plans leaves only")
        }
    }
}

/// Materialize what `dsh plugin add <package>` leaves in an instance's profile:
/// the profile's dependency entry plus the package under `node_modules`. For a
/// skin that package is a `dsh.client`-only client plugin (the kind the
/// launcher has to mount itself); for a plugin it also carries its own patch
/// file, which is where the toggleable row ids come from.
///
/// The install itself is pnpm run through the vendored DSH runtime, which a
/// throwaway root has no copy of — so the walk writes the post-install state
/// and then drives every launcher-owned step after it (the dependency the
/// Library projects from, the patch compile, the toggle, the insert row) for
/// real.
fn materialize_profile_package(instance: &InstanceManifest, package: &str, skin: bool) {
    let profile = DshAdapter::profile_dir(instance);
    let node_modules = profile.join("node_modules").join(package);
    std::fs::create_dir_all(&node_modules).unwrap();

    let manifest_path = profile.join("package.json");
    let mut manifest: serde_json::Value = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "name": "dsh-profile-web", "private": true }));
    let mut deps = manifest
        .get("dependencies")
        .and_then(|d| d.as_object())
        .cloned()
        .unwrap_or_default();
    deps.insert(package.to_string(), serde_json::json!("1.0.0"));
    manifest["dependencies"] = serde_json::Value::Object(deps);
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();

    // A skin declares `dsh.client` and no `dsh.bundle` — the shape the launcher
    // classifies as a Theme and mounts through an insert row. A plugin declares
    // `dsh.bundle.patch`, the row ids the patch layer can toggle.
    let (body, patch) = if skin {
        (
            serde_json::json!({
                "name": package, "version": "1.0.0", "main": "index.js",
                "dsh": { "client": { "entry": "./dist/index.js" } },
            }),
            None,
        )
    } else {
        (
            serde_json::json!({
                "name": package, "version": "1.0.0", "main": "index.js",
                "dsh": { "bundle": { "patch": "cordis.patch.yml" } },
            }),
            Some(format!("- insert:\n    - id: acceptance-toolbox\n      name: {package}\n")),
        )
    };
    std::fs::write(
        node_modules.join("package.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
    if let Some(patch) = patch {
        std::fs::write(node_modules.join("cordis.patch.yml"), patch).unwrap();
    }
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

#[test]
#[ignore = "end-to-end acceptance: needs git + node, and github for the one skill row. Run: cargo test -p dsh-adapter --test acceptance_e2e -- --ignored --nocapture"]
fn acceptance_walk_instance_to_import() {
    let mirror = std::env::var("AHL_WALK_MIRROR").is_ok();
    let logs = Logs::new();
    let root = workdir("walk");
    let paths = AppPaths::rooted_at(root.clone(), false);
    // One runtime for the whole walk: `spawn_child` streams the child's output
    // and watches for its exit from tasks on the runtime it was called on, so
    // the runtime has to outlive the child. An ephemeral `rt()` per call is
    // right for the self-contained async helpers and wrong here.
    let rt = rt();

    for tool in ["git", "node"] {
        let present = std::process::Command::new(tool).arg("--version").output().is_ok();
        // Not a skip: the launcher cannot run DSH, clone a fixture, or install
        // an MCP without these. A machine missing one cannot be accepted.
        assert!(present, "{tool} is not on PATH — the launcher cannot run without it");
    }

    eprintln!("acceptance walk → {}", root.display());
    eprintln!("github mirror: {}", if mirror { "on" } else { "off (AHL_WALK_MIRROR=1 to enable)" });
    paths.ensure_dirs().expect("create the app root");

    // ---- 1. create an instance ------------------------------------------
    step(1, "create an instance");
    let instance = InstanceManifest::create(&paths, "Acceptance Walk").expect("create instance");
    let file = paths.instance_file(&instance.id);
    assert!(file.is_file(), "no manifest at {}", file.display());
    assert!(Path::new(&instance.workspace).is_dir(), "no workspace at {}", instance.workspace);
    let listed = InstanceManifest::list(&paths).expect("list instances");
    assert_eq!(listed.len(), 1, "the new instance must be the only one");
    assert_eq!(listed[0].id, instance.id);
    ok(format!("instance {} at {}", instance.id, file.display()));

    // ---- 2. install the four kinds the Market ships ---------------------
    step(2, "install skill / MCP / skin / plugin");
    let skill_entry = market::bundled_skills()
        .plugins
        .into_iter()
        .find(|p| p.key() == SKILL_KEY)
        .unwrap_or_else(|| {
            panic!("the bundled skill catalog no longer carries {SKILL_KEY} — pick another entry")
        });
    let mut plan = vec![skill_entry.clone()];
    let mut instance = install_leaf(&paths, &instance, &skill_entry, mirror, &logs);

    // The skill row is the one that lands from the network: assert the landed
    // bytes are what the recorded hash describes, since that hash *is* the
    // update signal the whole skill-update flow compares against.
    let skill_dir = Path::new(&instance.workspace)
        .join("skills")
        .join(instance.skills[0].id.replace('/', "-"));
    let landed = file_sha256(&skill_dir.join("SKILL.md")).expect("hash the landed SKILL.md");
    assert_eq!(instance.skills[0].hash, landed, "the recorded hash must describe what landed");

    // The MCP row installs from a local git fixture, so this step needs no
    // network — the same shallow-clone → fingerprint → entry path a real
    // source-run takes.
    let repo = root.join("fixtures").join("mcp-repo");
    make_repo(
        &repo,
        &[
            ("package.json", r#"{"name":"walk-mcp-srv","version":"1.0.0","main":"server.js"}"#),
            ("server.js", MCP_SRV),
        ],
    );
    let (mcp_launch, _base, how) = rt
        .block_on(install_local(
            &repo.to_string_lossy(),
            &root.join("fixtures").join("mcp-clone"),
            None,
            None,
            logs.sink(),
        ))
        .unwrap_or_else(|e| panic!("local MCP install failed: {e}"));
    assert_eq!(how, "deterministic", "a package.json repo must not need AI to resolve");
    let mcp_entry = RegistryPlugin {
        kind: ContentKind::Mcp,
        owner: "acceptance".into(),
        name: MCP_SERVER.into(),
        transport: Some("stdio".into()),
        command: Some(mcp_launch.command.clone()),
        args: Some(mcp_launch.args.clone()),
        env: Some(mcp_launch.env.clone()),
        // What a catalog MCP entry carries so the export has a re-install
        // source; the local clone stands in for what npm would fetch.
        npm: Some("@acceptance/mcp-srv".into()),
        ..RegistryPlugin::default()
    };
    plan.push(mcp_entry.clone());
    instance = install_leaf(&paths, &instance, &mcp_entry, mirror, &logs);

    let skin_entry = RegistryPlugin {
        kind: ContentKind::Theme,
        owner: "acceptance".into(),
        name: "walk-skin".into(),
        npm: Some(PKG_SKIN.into()),
        url: "https://github.com/acceptance/walk-skin".into(),
        ..RegistryPlugin::default()
    };
    plan.push(skin_entry.clone());
    instance = install_leaf(&paths, &instance, &skin_entry, mirror, &logs);

    let plugin_entry = RegistryPlugin {
        kind: ContentKind::Plugin,
        owner: "acceptance".into(),
        name: "toolbox".into(),
        npm: Some(PKG_PLUGIN.into()),
        url: "https://github.com/acceptance/toolbox".into(),
        ..RegistryPlugin::default()
    };
    plan.push(plugin_entry.clone());
    instance = install_leaf(&paths, &instance, &plugin_entry, mirror, &logs);

    // A plugin's Enable/Disable writes *and removes* patch rows in the profile.
    // Disabling it here proves the toggle sticks through a cold re-read, and
    // re-running the skin compile afterwards proves the two patch writers do
    // not eat each other's rows.
    DshAdapter::set_plugin_enabled(&instance, PKG_PLUGIN, false).expect("disable the plugin");
    sync_skin_patch(&instance, &instance.skin_packages).expect("recompile the skin patch");
    instance = InstanceManifest::get(&paths, &instance.id).expect("re-read the instance");
    let plugin_row = DshAdapter::installed_plugins(&instance)
        .into_iter()
        .find(|p| p.name == PKG_PLUGIN)
        .expect("the plugin must project");
    assert!(!plugin_row.enabled, "the plugin was disabled; the patch row must still say so");
    assert!(plugin_row.toggleable, "a plugin with bundle rows must be switchable");
    ok("plugin toggle survives a cold re-read and the skin recompile");

    // ---- 3. Library sees all four, from disk ----------------------------
    step(3, "Library projections, re-read cold");
    let fresh = InstanceManifest::get(&paths, &instance.id).expect("re-read the instance");
    assert_eq!(fresh.skills.len(), 1, "one skill recorded");
    assert_eq!(fresh.mcp.len(), 1, "one MCP recorded");
    assert_eq!(fresh.skin_packages.len(), 1, "one skin recorded");
    let plugins = DshAdapter::installed_plugins(&fresh);
    assert!(
        plugins.iter().any(|p| p.name == PKG_SKIN && p.enabled && p.toggleable),
        "the mounted skin must project as an enabled, switchable Theme: {plugins:?}"
    );
    assert!(plugins.iter().any(|p| p.name == PKG_PLUGIN), "the plugin must project");
    // The MCP's health snapshot is what the Library badge reads.
    let runtime = launcher_core::load_runtime(&paths.mcp_runtime_file(&fresh.id, MCP_SERVER));
    assert_eq!(runtime.state, MCP_STATE_OK, "the MCP snapshot must survive a re-read");
    assert!(skill_dir.join("SKILL.md").is_file(), "the skill's files must be on disk");
    ok(format!("{} plugin row(s), {} skill(s), {} mcp(s), {} skin(s)",
        plugins.len(), fresh.skills.len(), fresh.mcp.len(), fresh.skin_packages.len()));

    // ---- 4. launch it, under the ledger ---------------------------------
    step(4, "launch (launcher half: spawn → logs → ready → stop)");
    let ledger = PidLedger::open(paths.pid_ledger());
    let node = which_node();
    let mut cmd = tokio::process::Command::new(&node);
    cmd.arg("-e")
        .arg(HTTP_STANDIN)
        .current_dir(&instance.workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let exited: Arc<Mutex<Option<launcher_core::ProcessState>>> = Arc::new(Mutex::new(None));
    let on_exit: ExitSink = {
        let slot = exited.clone();
        Arc::new(move |state| {
            if let Ok(mut s) = slot.lock() {
                *s = Some(state);
            }
        })
    };
    let mut handle = rt
        .block_on(spawn_child_with_exit(cmd, logs.sink(), Some(on_exit)))
        .expect("spawn the child");
    let pid = handle.pid;
    assert_eq!(handle.state().status, ProcessStatus::Starting, "a fresh child starts as Starting");

    // The startup zombie sweep runs before every launch. A tree this launcher
    // is still managing must survive it — otherwise a second launcher would
    // kill a healthy harness.
    ledger.record(&instance.id, pid);
    assert!(sweep_leftover(&ledger).is_empty(), "the sweep reaped a live launcher's own tree");
    ok(format!("pid {pid} recorded and left alone by the sweep"));

    // Readiness comes off the child's own stdout, the way the launcher reads
    // DSH's URL line out of the log stream.
    let line = logs
        .wait_line(Duration::from_secs(15), |l| l.starts_with("listening "))
        .expect("the child never reported its port on stdout");
    let port: u16 = line.trim_start_matches("listening ").trim().parse().expect("a port");
    assert!(
        rt.block_on(wait_for_port(port, Duration::from_secs(5))),
        "port {port} was reported but never accepted a connection"
    );
    handle.set_status(ProcessStatus::Running);
    assert_eq!(handle.state().status, ProcessStatus::Running);
    ok(format!("ready on 127.0.0.1:{port}, status Running"));

    rt.block_on(handle.stop()).expect("stop the child");
    // do_stop's bookkeeping: the row goes, and the whole tree dies with it.
    ledger.forget(pid);
    assert!(ledger.read().is_empty(), "the ledger must be empty after stop");
    assert!(eventually(Duration::from_secs(5), || !pid_alive(pid)), "stop left pid {pid} alive");
    let exit_state = exited.lock().unwrap().clone().expect("the exit callback must fire");
    assert_eq!(exit_state.status, ProcessStatus::Stopped, "a stopped child reports Stopped");
    ok("stopped, reaped, and the ledger row cleared");

    // ---- 5. usage lands in the ledger -----------------------------------
    step(5, "usage ledger");
    let ledger = UsageLedger::open(&paths.db_file()).expect("open the usage db");
    let now = launcher_core::now_secs();
    let known = NewUsageRecord {
        instance_id: instance.id.clone(),
        timestamp: Some(now),
        model: "deepseek-chat".into(),
        input_tokens: 100,
        output_tokens: 20,
        total_tokens: None,
        cost: Some(0.25),
        api_key_alias: "default".into(),
        request_id: Some("walk-req-1".into()),
    };
    let mut unknown = known.clone();
    unknown.request_id = Some("walk-req-2".into());
    unknown.input_tokens = 200;
    unknown.output_tokens = 40;
    unknown.model = "no-such-model-xyz".into();
    unknown.cost = None;

    assert!(ledger.record(known.clone()).unwrap().is_some(), "the first request records");
    assert!(ledger.record(unknown).unwrap().is_some(), "the second request records");
    // The proxy retries; the same request id must not be counted twice.
    assert!(ledger.record(known.clone()).unwrap().is_none(), "a duplicate request id must not double-count");

    let mut foreign = NewUsageRecord { request_id: Some("walk-req-3".into()), ..known.clone() };
    foreign.instance_id = "another-instance".into();
    assert!(ledger.record(foreign).unwrap().is_some());

    let mine = ledger.summary(Some(&instance.id), None, None, 0, now + 60).unwrap();
    assert_eq!(mine.requests, 2, "only this instance's requests");
    assert_eq!(mine.total_tokens, 100 + 20 + 200 + 40);
    assert_eq!(mine.by_instance.len(), 1, "another instance's rows must not leak into this view");
    assert_eq!(mine.cost_known_records, 1, "a reported cost is known");
    assert_eq!(mine.unknown_cost_records, 1, "an unpriced model must be flagged unknown, not zero");
    let all = ledger.summary(None, None, None, 0, now + 60).unwrap();
    assert_eq!(all.requests, 3, "the unfiltered view sees every instance");
    ok(format!("{} request(s) for this instance, {} across all, 1 unpriced", mine.requests, all.requests));

    // ---- 6. export the environment --------------------------------------
    step(6, "export the environment");
    let manifest = EnvironmentManifest {
        format: ENVIRONMENT_FORMAT.into(),
        format_version: ENVIRONMENT_FORMAT_VERSION,
        exported_at: launcher_core::now_secs(),
        name: instance.name.clone(),
        description: format!("Environment exported from {}", instance.name),
        compatible_with: instance.runtime.version.clone(),
        source: EnvironmentSource {
            instance_id: instance.id.clone(),
            instance_name: instance.name.clone(),
            runtime: instance.runtime.version.clone(),
        },
        items: plan.clone(),
        exports: plan
            .iter()
            .map(|item| ExportedItem {
                key: item.key(),
                kind: item.kind,
                name: item.name.clone(),
                source: item.install_spec(),
                version: None,
            })
            .collect(),
    };
    // Hash the *canonical* form, exactly as `commands::environment::manifest_checksum`
    // does (this test drives the adapter, so that private fn is out of reach — the
    // two must stay in step). Canonicalising means a round trip through `Value`,
    // whose objects are `BTreeMap`s and therefore re-emit with sorted keys: the
    // manifest's map-valued fields are `HashMap`s with a per-instance randomized
    // order, so hashing the struct directly would make the checksum depend on
    // which instance serialized it.
    let bytes = serde_json::to_vec(&serde_json::to_value(&manifest).expect("canonicalize"))
        .expect("serialize the manifest");
    let checksum = sha256_hex(&bytes);
    // The four checks the importer's preflight makes, made here on the bytes it
    // would be handed: right format, right version, something to install, and a
    // checksum that still matches after the round trip.
    assert_eq!(manifest.format, ENVIRONMENT_FORMAT, "not a DSH environment package");
    assert_eq!(manifest.format_version, ENVIRONMENT_FORMAT_VERSION, "unsupported package version");
    assert!(!manifest.items.is_empty(), "a package with no installable items is refused");
    let reparsed: EnvironmentManifest = serde_json::from_slice(&bytes).expect("parse it back");
    let reserialized = serde_json::to_vec(&serde_json::to_value(&reparsed).expect("canonicalize"))
        .expect("re-serialize the manifest");
    assert_eq!(
        sha256_hex(&reserialized),
        checksum,
        "the checksum must survive the round trip the importer verifies\n  stored:   {}\n  reparsed: {}",
        String::from_utf8_lossy(&bytes),
        String::from_utf8_lossy(&reserialized)
    );
    assert_eq!(
        reparsed.items.iter().map(|i| i.key()).collect::<Vec<_>>(),
        plan.iter().map(|i| i.key()).collect::<Vec<_>>(),
        "every item key must survive"
    );
    let sourceless: Vec<String> = reparsed
        .items
        .iter()
        .filter(|i| i.install_spec().is_empty())
        .map(|i| i.key())
        .collect();
    assert!(sourceless.is_empty(), "no re-install source for: {sourceless:?}");
    ok(format!("{} item(s), checksum {}…", reparsed.items.len(), &checksum[..12]));

    // ---- 7. import it into a fresh instance -----------------------------
    step(7, "import into a fresh instance");
    let mut target = InstanceManifest::create(&paths, "Imported Walk").expect("create the import");
    assert_ne!(target.id, instance.id, "an import lands in its own instance");
    for item in &reparsed.items {
        // An import re-resolves each leaf rather than pointing at the exporting
        // instance's files: the MCP fixture is cloned fresh, the way a real
        // import re-installs from its source.
        let leaf = if item.kind == ContentKind::Mcp {
            let (replay, _base, _how) = rt
                .block_on(install_local(
                    &repo.to_string_lossy(),
                    &root.join("import").join("mcp-clone"),
                    None,
                    None,
                    logs.sink(),
                ))
                .unwrap_or_else(|e| panic!("re-install the MCP leaf: {e}"));
            RegistryPlugin {
                command: Some(replay.command),
                args: Some(replay.args),
                env: Some(replay.env),
                ..item.clone()
            }
        } else {
            item.clone()
        };
        target = install_leaf(&paths, &target, &leaf, mirror, &logs);
    }

    let imported = InstanceManifest::get(&paths, &target.id).expect("re-read the import");
    assert_eq!(imported.skills.len(), 1, "the skill leaf must land again");
    assert_eq!(imported.mcp.len(), 1, "the MCP leaf must land again");
    assert_eq!(imported.skin_packages.len(), 1, "the skin leaf must land again");
    assert!(
        DshAdapter::installed_plugins(&imported).iter().any(|p| p.name == PKG_PLUGIN),
        "the plugin leaf must land again"
    );
    // The two instances keep separate workspaces: an import must not write into
    // the exporting instance.
    assert_ne!(imported.workspace, instance.workspace);
    let imported_skill = Path::new(&imported.workspace)
        .join("skills")
        .join(imported.skills[0].id.replace('/', "-"))
        .join("SKILL.md");
    assert!(imported_skill.is_file(), "no SKILL.md at {}", imported_skill.display());
    assert!(imported_skill.starts_with(Path::new(&imported.workspace)), "escaped the workspace");
    assert_eq!(InstanceManifest::list(&paths).unwrap().len(), 2, "both instances are listed");
    ok(format!("instance {} mirrors all four kinds", imported.id));

    eprintln!("\nacceptance walk: PASS — 7/7 steps on {}", root.display());
    // Close the usage db first: `remove_dir_all` stops at the first file a live
    // handle holds, and an open SQLite connection is exactly that — the walk
    // would otherwise leave its root (and its temp noise) behind on a pass.
    drop(ledger);
    match std::fs::remove_dir_all(&root) {
        Ok(()) => eprintln!("acceptance walk: root removed"),
        Err(e) => eprintln!("acceptance walk: could not remove {} ({e})", root.display()),
    }
}

/// The node the launcher would resolve, from PATH. The walk's shell has no
/// bundled runtime to prefer, and a missing node already failed the baseline.
fn which_node() -> PathBuf {
    if let Ok(out) = std::process::Command::new("node").arg("--version").output() {
        if out.status.success() {
            return PathBuf::from("node");
        }
    }
    panic!("node is not on PATH");
}
