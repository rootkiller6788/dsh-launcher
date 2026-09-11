// GUI walk — a real window, a real click on Launch, and DSH appearing in it.
//
//   cargo test -p ai-harness-launcher --test gui_launch_e2e -- --ignored --nocapture
//
// The acceptance walk in `crates/dsh-adapter/tests/acceptance_e2e.rs` drives the
// launcher's stages at the layer the commands call into. This one drives the
// layer above it: WebDriver attaches to the actual Tauri window, finds the
// Launch button in the rendered DOM, clicks it, and waits for the DSH view to
// appear. That is the half the other walk explicitly leaves out — it can assert
// the URL line and the process state, but not that the UI reacts to them.
//
// Prerequisites. The test fails naming the one that is missing:
//
//   tauri-driver    cargo install tauri-driver --locked
//   msedgedriver    must match the installed WebView2 runtime. List the runtime
//                   version under
//                     %ProgramFiles(x86)%\Microsoft\EdgeWebView\Application
//                   then download that version's driver from
//                     https://msedgedriver.microsoft.com/<version>/edgedriver_win64.zip
//                   and point AHL_MSEDGEDRIVER at the .exe (or put it in
//                   %LOCALAPPDATA%\ahl-e2e\msedgedriver.exe).
//   node, pnpm      the app is a debug build, so it loads the frontend from the
//                   Vite dev server; the walk starts one on :1420 if that port
//                   is free and reuses it otherwise.
//   no leftovers    an interrupted run can leave a tauri-driver on :4444 or a
//                   launcher window holding the single-instance mutex, and both
//                   make the driver report only "Chrome instance exited". The
//                   walk checks for them first and names the culprit.
//
// Nothing outside a temp root is touched. The app runs with
// `AHL_HOME=<temp root>` so its settings, instances, logs and pid ledger are all
// throwaway; `DSH_CLI_BIN` points at a stub that serves a page and prints DSH's
// URL line, so no real harness boots and no API call is made; and the provider
// key is a dummy stored under the `gui-walk` account, deleted on the way out.
// The root is removed on success and left behind (with its path printed) on
// failure, so a failed run leaves the evidence in place.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use launcher_core::provider::{ProviderProfile, ProviderVault};
use launcher_core::{AppPaths, AppSettings, InstanceManifest};

/// The provider the walk's instance points at. The name is deliberately ours:
/// the key below is stored under `provider:gui-walk` in the OS credential store,
/// which is what keeps it away from any real provider the user has configured.
const PROVIDER_ID: &str = "gui-walk";
const PROVIDER_KEY: &str = "sk-gui-walk-not-a-real-key";
const INSTANCE_NAME: &str = "GUI Walk";

/// The label the Launch button carries. The walk pins the UI language to English
/// (see `stage_root`) so this is the string that renders, rather than whichever
/// language the machine's locale would otherwise pick.
const LAUNCH_LABEL: &str = "Launch DSH";

/// Title of the workspace iframe that shows DSH (`App.tsx`). The launcher has no
/// test ids anywhere, so this title — and the shell panel's state class — are the
/// stable signals for "the DSH view is on screen".
const WORKSPACE_IFRAME: &str = "iframe[title=\"DeepSeek Harness Workspace\"]";

/// A stand-in for the harness. `detect()` reads the version out of
/// `<cli>/package.json` next to the bin, so the stub ships one; the launcher then
/// spawns `<node> <this> web --host 127.0.0.1 --port 0`, which this answers the
/// way DSH does: print the URL line, serve a page, stay up until killed.
const STUB_DSH: &str = r#"const http = require('http');
const marker = 'DSH-Stub-Marker';
const argv = process.argv;
const portArg = argv.indexOf('--port');
const want = portArg >= 0 ? Number(argv[portArg + 1]) : 0;
const srv = http.createServer((_q, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end('<!doctype html><title>DSH stub</title><h1>' + marker + '</h1>');
});
srv.listen(want, '127.0.0.1', () => {
  // Exactly the line `parse_dsh_url` looks for.
  console.log('dsh web: http://127.0.0.1:' + srv.address().port);
});
process.on('SIGTERM', () => process.exit(0));
setInterval(() => {}, 1 << 30);
"#;

/// Marker the stub serves, asserted through the iframe's own src to prove the
/// frame really loaded the harness page and not just a well-formed URL.
const STUB_MARKER: &str = "DSH-Stub-Marker";

const DEV_URL: &str = "http://localhost:1420";
/// Where tauri-driver listens (its default). The app is launched by the driver
/// as its own child, so the temp root and stub CLI reach it through the driver's
/// environment.
const DRIVER_ADDR: &str = "127.0.0.1:4444";

fn main_walk() {
    let exe = env!("CARGO_BIN_EXE_ai-harness-launcher");
    let tauri_driver = require_tool(
        "tauri-driver",
        Some("AHL_TAURI_DRIVER"),
        "install it with:  cargo install tauri-driver --locked",
    );
    let edge_driver = require_edge_driver();
    step(
        0,
        &format!(
            "prerequisites: tauri-driver, msedgedriver, node, a debug build at {}",
            exe
        ),
    );
    ok(&format!("tauri-driver  {}", tauri_driver.display()));
    ok(&format!("msedgedriver  {}", edge_driver.display()));
    ok(&format!("app           {exe}"));

    let root = temp_root();
    let mut run = Run {
        provider: None,
        driver: None,
        session: None,
        dev_server: None,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let outcome = rt.block_on(async { drive(&mut run, &root, exe, &tauri_driver, &edge_driver).await });

    // Teardown runs on both paths: the session first (it closes the app), then
    // the driver, then the dev server, then the dummy credential, then the root.
    // The session delete is the one awaitable step, so it gets the runtime the
    // walk already built rather than a second one.
    if let Some(session) = run.session.take() {
        let _ = rt.block_on(session.delete());
    }
    if let Some(mut driver) = run.driver.take() {
        kill_tree(&mut driver);
    }
    // Backstop. The session delete above closes the app, but a walk that failed
    // before a session existed never got that far, and an app that hangs on the
    // way out ignores it. Either one leaves a process holding the single-instance
    // mutex, and every run after it dies with the driver's opaque "Chrome
    // instance exited" — so the stray is killed here rather than left for the
    // next run to trip over. The preflight in `drive` guarantees anything
    // matching now was started by this walk.
    kill_launcher_apps();
    if let Some(mut dev) = run.dev_server.take() {
        kill_tree(&mut dev);
    }
    if let Some(vault) = run.provider.take() {
        vault.delete(PROVIDER_ID).ok();
    }

    match outcome {
        Ok(()) => {
            eprintln!("\ngui walk: PASS — window, click, DSH view");
            match std::fs::remove_dir_all(&root) {
                Ok(()) => eprintln!("gui walk: root removed"),
                Err(e) => eprintln!("gui walk: could not remove {} ({e})", root.display()),
            }
        }
        Err(e) => {
            eprintln!("\ngui walk: FAIL — {e}");
            eprintln!("gui walk: leaving {} in place for inspection", root.display());
            std::process::exit(1);
        }
    }
}

/// Everything the walk owns and has to hand back.
struct Run {
    /// The vault the dummy provider key was written through, so teardown can
    /// delete it from the credential store as well as from the temp root.
    provider: Option<ProviderVault>,
    driver: Option<Child>,
    session: Option<Session>,
    dev_server: Option<Child>,
}

async fn drive(
    run: &mut Run,
    root: &Path,
    exe: &str,
    tauri_driver: &Path,
    edge_driver: &Path,
) -> Result<(), String> {
    let paths = stage_root(root)?;
    ok(&format!("data root    {}", root.display()));

    let child_env: Vec<(String, String)> = vec![
        ("AHL_HOME".into(), root.display().to_string()),
        (
            "DSH_CLI_BIN".into(),
            root.join("fixtures/stub-dsh/apps/cli/lib/bin.js")
                .display()
                .to_string(),
        ),
    ];
    for (k, v) in &child_env {
        ok(&format!("{k}={v}"));
    }
    run.provider = Some(ProviderVault::new(paths.clone()));

    // ---- 3. the frontend the debug build loads ---------------------------
    step(3, "frontend");
    if !port_open("127.0.0.1:1420") {
        let mut dev = Command::new("cmd")
            .args(["/C", "pnpm", "dev"])
            .current_dir(run_staging_from(exe))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start `pnpm dev`: {e}"))?;
        wait_for_port("127.0.0.1:1420", Duration::from_secs(90), "the Vite dev server")
            .inspect_err(|_| kill_tree(&mut dev))?;
        run.dev_server = Some(dev);
        ok(&format!("started a dev server on {DEV_URL}"));
    } else {
        ok(&format!("reusing the dev server already on {DEV_URL}"));
    }

    // ---- 4. attach to the real window ------------------------------------
    step(4, "tauri-driver session");
    // Two leftovers from an interrupted run both surface as the same opaque
    // driver error — "session not created: Chrome instance exited", which names
    // neither of them — so they are checked for, and reported, before the driver
    // starts:
    //
    //   * a lingering tauri-driver owns :4444. This run's own driver then fails
    //     to bind and `wait_for_port` still succeeds, so the `POST /session`
    //     below is answered by the *stale* driver.
    //   * a lingering launcher owns the single-instance mutex.
    //     `tauri_plugin_single_instance` (`lib.rs`) takes it process-wide, not
    //     per `AHL_HOME`, so the app the driver launches is not a second
    //     launcher at all: it hands off to the survivor and exits without ever
    //     opening a window.
    if port_open(DRIVER_ADDR) {
        return Err(format!(
            "something is already listening on {DRIVER_ADDR} — almost certainly a tauri-driver \
             left behind by an interrupted run. Find and kill it (`tasklist | findstr \
             tauri-driver`, then `taskkill /PID <pid> /T /F`) and re-run."
        ));
    }
    let strays = launcher_pids();
    if !strays.is_empty() {
        let pids: Vec<String> = strays.iter().map(u32::to_string).collect();
        return Err(format!(
            "ai-harness-launcher.exe is already running (pid {}). The single-instance plugin \
             takes a process-wide mutex, so the window this walk launches would hand off to that \
             process and exit before the driver could attach. Close it (`taskkill /PID {} /T /F`) \
             and re-run.",
            pids.join(", "),
            pids.join(" /PID ")
        ));
    }
    let mut driver = Command::new(tauri_driver)
        .arg("--native-driver")
        .arg(edge_driver)
        .envs(child_env.iter().cloned())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start tauri-driver: {e}"))?;
    wait_for_port(DRIVER_ADDR, Duration::from_secs(30), "tauri-driver")
        .inspect_err(|_| kill_tree(&mut driver))?;
    run.driver = Some(driver);

    let session = Session::start(exe, DRIVER_ADDR).await?;
    let handle = session.window_handle().await?;
    ok(&format!("session {} on window {handle}", session.id));
    // The driver is the app's parent, so it passes its own environment on: the
    // temp root and the stub CLI are what the app boots with.
    //
    // Wait for the UI to mount before looking for anything: the window exists
    // well before React has rendered into it, and every failure below is only
    // diagnosable if the URL it was on is part of the report.
    let mounted = Instant::now();
    let deadline = mounted + Duration::from_secs(60);
    loop {
        let probe = session
            .js(
                "const root = document.querySelector('#root');\n\
                 return { href: location.href, title: document.title,\n\
                           ready: document.readyState,\n\
                           nodes: root ? root.children.length : -1 }",
            )
            .await?;
        let nodes = probe.get("nodes").and_then(|n| n.as_i64()).unwrap_or(-1);
        let href = probe.get("href").and_then(|h| h.as_str()).unwrap_or("?");
        if nodes > 0 {
            ok(&format!(
                "app rendered: {href} ({} node(s) under #root, {}ms)",
                nodes,
                mounted.elapsed().as_millis()
            ));
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the app window never rendered its UI — it is on {href} with {nodes} node(s) \
                 under #root, after 60s.\n\
                 A blank window means the frontend never loaded: a debug build serves it from \
                 {DEV_URL}, so the Vite dev server has to be up and serving.\n{}",
                session.dump(root).await
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ---- 5. find the button and click it ---------------------------------
    step(5, &format!("click \"{LAUNCH_LABEL}\""));
    let button = match session
        .find_clickable(&format!("//button[normalize-space()='{LAUNCH_LABEL}']"))
        .await
    {
        Ok(button) => button,
        Err(e) => {
            // The window's text is what makes this failure diagnosable: it says
            // which page the app is actually on.
            let text = session.dump(root).await;
            return Err(format!(
                "no \"{LAUNCH_LABEL}\" button in the window — {e}\n{text}"
            ));
        }
    };
    let disabled = session.attr(&button, "disabled").await.unwrap_or_default();
    if disabled == "true" {
        let text = session.dump(root).await;
        return Err(format!(
            "the \"{LAUNCH_LABEL}\" button is disabled — the app has no active instance.\n{text}"
        ));
    }
    session.click(&button).await?;
    ok("clicked");

    // ---- 6. the DSH view appears ----------------------------------------
    step(6, "the DSH view appears");
    // What "the DSH view is on screen" means here, precisely: the workspace shell
    // panel is the active one, and its iframe carries a live harness URL. That
    // URL is set by exactly one thing — the `dsh-url` event, which the launcher
    // emits only from its readiness path once the child has reported a URL and
    // the port answers. So an active panel with a serving URL *is* the running
    // signal; asserting the word "running" instead would read the manage panel's
    // status line, which the launch has just switched away from (and which
    // `innerText` excludes anyway, that panel being `visibility: hidden`).
    let probe = format!(
        "const f = document.querySelector('{WORKSPACE_IFRAME}');\n\
         const panel = f && f.closest('.shell-panel');\n\
         return {{ src: f ? f.getAttribute('src') : null,\n\
                   active: !!panel && panel.classList.contains('shell-panel-active'),\n\
                   hidden: !!panel && panel.getAttribute('aria-hidden') === 'true' }}"
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = String::from("(nothing probed yet)");
    loop {
        // Checked at the top so the failure report carries the *previous*
        // probe's state rather than one last unreported attempt.
        if Instant::now() >= deadline {
            let text = session.dump(root).await;
            return Err(format!(
                "the DSH view never appeared within 120s (last: {last})\n{text}"
            ));
        }
        let state = session.js(&probe).await?;
        let src = state.get("src").and_then(|s| s.as_str()).unwrap_or("");
        let active = state.get("active").and_then(|b| b.as_bool()).unwrap_or(false);
        if let Some(port) = local_url_port(src) {
            if active && !src.is_empty() {
                ok(&format!("workspace iframe active, src={src}"));
                // Fetch the frame's URL: the view is only really DSH if that URL
                // serves DSH. This is the assertion that a URL-shaped string in
                // the DOM cannot fake.
                let body = reqwest::get(src)
                    .await
                    .map_err(|e| format!("could not fetch the DSH url {src}: {e}"))?
                    .text()
                    .await
                    .map_err(|e| format!("could not read the DSH url {src}: {e}"))?;
                if !body.contains(STUB_MARKER) {
                    return Err(format!(
                        "the URL in the workspace iframe ({src}) is not serving the harness page"
                    ));
                }
                ok(&format!("{src} serves the harness page (port {port})"));
                return Ok(());
            }
        }
        last = format!(
            "src={src:?} active={active} hidden={}",
            state.get("hidden").and_then(|b| b.as_bool()).unwrap_or(true)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The dev server serves `apps/desktop`, which is the crate's grandparent's
/// sibling — derived from the built exe so a relocated target dir still works.
fn run_staging_from(exe: &str) -> PathBuf {
    // …\target\debug\ai-harness-launcher.exe → …\apps\desktop
    Path::new(exe)
        .ancestors()
        .nth(3)
        .map(|p| p.join("apps").join("desktop"))
        .filter(|p| p.join("package.json").is_file())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."))
}

/// Lay out the throwaway data root: settings (English UI, our instance active),
/// the provider the instance points at, the instance itself, and the stub CLI.
fn stage_root(root: &Path) -> Result<AppPaths, String> {
    let paths = AppPaths::rooted_at(root.to_path_buf(), false);
    paths
        .ensure_dirs()
        .map_err(|e| format!("could not create the data root: {e}"))?;

    let vault = ProviderVault::new(paths.clone());
    vault
        .set(
            &ProviderProfile {
                id: PROVIDER_ID.into(),
                name: "GUI Walk".into(),
                base_url: None,
                model: Some("stub".into()),
                models: vec!["stub".into()],
            },
            Some(PROVIDER_KEY),
        )
        .map_err(|e| format!("could not stage the provider key: {e}"))?;

    let mut instance =
        InstanceManifest::create(&paths, INSTANCE_NAME).map_err(|e| format!("create: {e}"))?;
    instance.provider_ref = PROVIDER_ID.into();
    instance
        .save(&paths.instance_file(&instance.id))
        .map_err(|e| format!("could not save the instance: {e}"))?;

    // `language = "en"` is what makes the Launch button's label the string
    // `LAUNCH_LABEL` holds, rather than whichever language the machine's locale
    // would otherwise pick.
    AppSettings {
        language: Some("en".into()),
        last_instance: Some(instance.id.clone()),
        ..AppSettings::default()
    }
    .save(&paths)
    .map_err(|e| format!("could not save settings: {e}"))?;

    // `<cli>/lib/bin.js` + `<cli>/package.json`, the layout `detect()` reads its
    // version from.
    let cli = root.join("fixtures/stub-dsh/apps/cli");
    std::fs::create_dir_all(cli.join("lib")).map_err(|e| format!("stub cli dirs: {e}"))?;
    std::fs::write(cli.join("lib/bin.js"), STUB_DSH).map_err(|e| format!("stub cli: {e}"))?;
    std::fs::write(
        cli.join("package.json"),
        serde_json::json!({ "name": "stub-dsh", "version": "9.9.9-gui-walk" }).to_string(),
    )
    .map_err(|e| format!("stub cli package.json: {e}"))?;
    Ok(paths)
}

// ---------------------------------------------------------------------------
// WebDriver
// ---------------------------------------------------------------------------

/// A W3C WebDriver session against tauri-driver. Only the calls this walk needs:
/// find, click, run script, read an attribute, closes.
struct Session {
    id: String,
    http: reqwest::Client,
    base: String,
}

impl Session {
    /// `tauri:options.application` is how tauri-driver learns which binary to
    /// launch; the rest of the capabilities are the W3C minimum.
    async fn start(exe: &str, driver: &str) -> Result<Self, String> {
        let http = reqwest::Client::new();
        let base = format!("http://{driver}");
        let body = serde_json::json!({
            "capabilities": { "alwaysMatch": {
                "tauri:options": { "application": exe },
            }},
        });
        let value = post(
            &http,
            &format!("{base}/session"),
            &body,
            "create the session",
        )
        .await?;
        let id = value
            .get("sessionId")
            .and_then(|s| s.as_str())
            .ok_or_else(|| format!("tauri-driver returned no sessionId: {value}"))?
            .to_string();
        Ok(Self { id, http, base })
    }

    fn url(&self, tail: &str) -> String {
        format!("{}/session/{}{tail}", self.base, self.id)
    }

    async fn window_handle(&self) -> Result<String, String> {
        let v = get(&self.http, &self.url("/window"), "read the window handle").await?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    /// Every match, in document order. The W3C `xpath` strategy is the only one
    /// on offer here — the launcher has no test ids, so text and titles are what
    /// selectors have to work with.
    async fn find_all(&self, xpath: &str) -> Result<Vec<String>, String> {
        let body = serde_json::json!({ "using": "xpath", "value": xpath });
        let v = post(&self.http, &self.url("/elements"), &body, "find elements").await?;
        let list = v
            .as_array()
            .ok_or_else(|| format!("expected a list of elements, got {v}"))?;
        Ok(list
            .iter()
            .filter_map(|e| e.get(ELEMENT_KEY).and_then(|i| i.as_str()).map(String::from))
            .collect())
    }

    async fn displayed(&self, element: &str) -> Result<bool, String> {
        let v = get(
            &self.http,
            &self.url(&format!("/element/{element}/displayed")),
            "check whether an element is displayed",
        )
        .await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    /// The match a user could actually click. The shell keeps the workspace
    /// panel mounted behind the manage panel (`styles.css`: a crossfade, not a
    /// remount) and that panel has its own Launch button, so a plain XPath can
    /// land on a button that is in the DOM but invisible — picking the first
    /// *displayed* match is what "click Launch" means.
    async fn find_clickable(&self, xpath: &str) -> Result<String, String> {
        let candidates = self.find_all(xpath).await?;
        let mut hidden = 0;
        for candidate in &candidates {
            if self.displayed(candidate).await.unwrap_or(false) {
                return Ok(candidate.clone());
            }
            hidden += 1;
        }
        Err(format!(
            "no displayed element for {xpath} ({hidden} hidden match(es), {} total)",
            candidates.len()
        ))
    }

    async fn click(&self, element: &str) -> Result<(), String> {
        post(
            &self.http,
            &self.url(&format!("/element/{element}/click")),
            &serde_json::json!({}),
            "click",
        )
        .await?;
        Ok(())
    }

    async fn attr(&self, element: &str, name: &str) -> Result<String, String> {
        let v = get(
            &self.http,
            &self.url(&format!("/element/{element}/attribute/{name}")),
            "read an attribute",
        )
        .await?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    /// `script` is a *function body* — W3C wraps it for us — so it has to return
    /// its value. Wrapping it in an IIFE would discard the return (the wrapper's
    /// value is only the IIFE's, and nothing returns that), which reads as every
    /// probe coming back `null`.
    async fn js(&self, script: &str) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({ "script": script, "args": [] });
        post(
            &self.http,
            &self.url("/execute/sync"),
            &body,
            "run a script",
        )
        .await
    }

    /// What the window shows, for a failure report. Never fails itself, and
    /// leaves a screenshot behind: a failed GUI walk is unreadable without
    /// seeing the window it was looking at.
    async fn dump(&self, dir: &Path) -> String {
        let shot = match self.screenshot().await {
            Ok(png) => {
                let file = dir.join("failure.png");
                match std::fs::write(&file, png) {
                    Ok(()) => format!("  screenshot: {}", file.display()),
                    Err(e) => format!("  (could not write the screenshot: {e})"),
                }
            }
            Err(e) => format!("  (could not take a screenshot: {e})"),
        };
        match self
            .js("return document.body ? document.body.innerText : '(no body)'")
            .await
        {
            Ok(v) => format!("{shot}\n  window text:\n{}", v.as_str().unwrap_or("(not a string)")),
            Err(e) => format!("{shot}\n  (could not read the window: {e})"),
        }
    }

    /// The window as PNG bytes.
    async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let v = get(&self.http, &self.url("/screenshot"), "take a screenshot").await?;
        let b64 = v
            .as_str()
            .ok_or_else(|| format!("screenshot is not a string: {v}"))?;
        base64_decode(b64)
    }

    async fn delete(self) -> Result<(), String> {
        let _ = self
            .http
            .delete(self.url(""))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// W3C's element key — a webdriver-over-JSON detail, spelled out so it is not a
/// mystery string at the call site.
const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

async fn post(
    http: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    what: &str,
) -> Result<serde_json::Value, String> {
    let res = http
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("could not {what}: {e}"))?;
    unwrap(res, what).await
}

async fn get(
    http: &reqwest::Client,
    url: &str,
    what: &str,
) -> Result<serde_json::Value, String> {
    let res = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("could not {what}: {e}"))?;
    unwrap(res, what).await
}

/// A WebDriver response carries its payload (or its error) under `value`.
async fn unwrap(res: reqwest::Response, what: &str) -> Result<serde_json::Value, String> {
    let status = res.status();
    let text = res.text().await.map_err(|e| format!("could not {what}: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("could not {what}: {status} {text} ({e})"))?;
    let value = value.get("value").cloned().unwrap_or(value);
    if !status.is_success() {
        let error = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown");
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        return Err(format!("could not {what}: {error}: {message}"));
    }
    Ok(value)
}

/// The port of a `http://127.0.0.1:<port>…` URL, if that is what this is. The
/// walk's stub picks a free port at runtime, so the port is learned, not known.
fn local_url_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Standard-alphabet base64. WebDriver returns screenshots that way and that is
/// the only base64 the walk ever sees, so a decoder beats a dependency.
fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    fn sextet(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let part = sextet(c).ok_or_else(|| format!("not base64: {:?}", c as char))?;
        acc = (acc << 6) | part;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Processes and files
// ---------------------------------------------------------------------------

/// Locate a tool: an explicit env override, then the well-known spot, then PATH.
/// A missing prerequisite is named with the command that installs it — the walk
/// is opt-in, so it should not fail quietly.
fn require_tool(name: &str, env_key: Option<&str>, how: &str) -> PathBuf {
    if let Some(key) = env_key {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
            eprintln!("gui walk: {key}={} is not a file, falling back to PATH", p.display());
        }
    }
    if let Some(p) = find_on_path(name) {
        return p;
    }
    panic!("{name} is not installed — {how}");
}

fn require_edge_driver() -> PathBuf {
    if let Ok(p) = std::env::var("AHL_MSEDGEDRIVER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
        panic!("AHL_MSEDGEDRIVER={} is not a file", p.display());
    }
    if let Some(p) = find_on_path("msedgedriver") {
        return p;
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("ahl-e2e").join("msedgedriver.exe");
        if p.is_file() {
            return p;
        }
    }
    panic!(
        "msedgedriver is not installed — it must match the WebView2 runtime in \
         %ProgramFiles(x86)%\\Microsoft\\EdgeWebView\\Application; download that version's \
         edgedriver_win64.zip from https://msedgedriver.microsoft.com/ and set AHL_MSEDGEDRIVER"
    );
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe = format!("{name}.exe");
    std::env::split_paths(&path)
        .flat_map(|dir| [dir.join(name), dir.join(&exe)])
        .find(|cand| cand.is_file())
}

fn temp_root() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("ahl-gui-walk-{}-{stamp}", std::process::id()))
}

/// Kill a process *and its tree*. `Child::kill` leaves `tauri-driver`'s
/// msedgedriver (and the app it launched) running, which would hold the window
/// and the temp root open for the rest of the session.
fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Pids of running launcher windows, via `tasklist` — the same tool the teardown
/// path already shells out to, and one that is simply absent off Windows (where
/// this walk cannot run anyway), so the answer degrades to "none".
fn launcher_pids() -> Vec<u32> {
    #[cfg(windows)]
    {
        let listing = Command::new("tasklist")
            .args([
                "/FI",
                "IMAGENAME eq ai-harness-launcher.exe",
                "/FO",
                "CSV",
                "/NH",
            ])
            .output();
        if let Ok(listing) = listing {
            // CSV rows are `"name","pid","session",…`. With no match, tasklist
            // prints a (localized) prose line to stdout instead, which carries no
            // quoted second field and so contributes nothing.
            let text = String::from_utf8_lossy(&listing.stdout);
            let pids: Vec<u32> = text
                .lines()
                .filter_map(|line| line.split("\",\"").nth(1))
                .filter_map(|pid| pid.trim_matches('"').parse().ok())
                .collect();
            return pids;
        }
    }
    Vec::new()
}

/// Kill any launcher window the walk left standing, tree and all.
fn kill_launcher_apps() {
    for pid in launcher_pids() {
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = pid;
    }
}

fn port_open(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    match addrs.next() {
        Some(a) => TcpStream::connect_timeout(&a, Duration::from_millis(400)).is_ok(),
        None => false,
    }
}

fn wait_for_port(addr: &str, timeout: Duration, what: &str) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if port_open(addr) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("{what} never came up on {addr} within {}s", timeout.as_secs()))
}

// ---------------------------------------------------------------------------
// The walk's running commentary — the printed trace is the report
// ---------------------------------------------------------------------------

fn step(n: u8, what: &str) {
    eprintln!("\n[{n}/7] {what}");
}

fn ok(what: &str) {
    eprintln!("      ✓ {what}");
}

#[test]
#[ignore = "GUI walk: needs tauri-driver + msedgedriver + a debug build; \
            cargo test -p ai-harness-launcher --test gui_launch_e2e -- --ignored --nocapture"]
fn gui_walk_launch_shows_the_dsh_view() {
    main_walk();
}
