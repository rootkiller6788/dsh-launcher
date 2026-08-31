#!/usr/bin/env node
// dsh-tauri sidecar — launches the REAL DSH Host (upstream `dsh web` web carrier)
// and reports readiness back to the Rust shell.
//
// Protocol (shared with the Phase 1 mock):
//   argv:    --port <port>          default 0  (0 = let the OS pick, report actual)
//            --retry-limit <n>      only used by --mock mode
//            --home <dir>           DSH_HOME override (default: host/runtime/.dsh-home)
//            --mock                 run the placeholder server instead of the real Host
//   stdout:  DSH_READY <port> <token>   once the Host's web carrier is listening
//   stdout:  DSH_ERROR <msg>        fatal startup failure
//   stdin:   closing = parent dropped us -> kill the Host tree and exit
//
// The real Host is `@deepseek-ai/dsh` (web profile). It prints
//   dsh web: http://127.0.0.1:<port>/?token=<token>
// when ready; we parse that line for the actual port + browser-trust token. The
// webview must load `/?token=<token>` or the Host returns HTTP 401.

import http from "node:http";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const RUNTIME_DIR = join(HERE, "runtime");
const DSH_BIN = join(RUNTIME_DIR, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
const DEFAULT_HOME = join(RUNTIME_DIR, ".dsh-home");
const DEFAULT_PORT_RETRY_LIMIT = 32;

/// Matches the readiness line the Host prints: `dsh web: http://127.0.0.1:<p>/?token=<t>`.
const WEB_URL_RE = /http:\/\/127\.0\.0\.1:(\d+)\/\?token=(\S+)/;

function parseArgs(argv) {
  let port = 0; // 0 => OS-assigned; the actual port comes back in DSH_READY.
  let retryLimit = DEFAULT_PORT_RETRY_LIMIT;
  let home = DEFAULT_HOME;
  let mock = false;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--port" && i + 1 < argv.length) port = Number(argv[i + 1]);
    else if (a.startsWith("--port=")) port = Number(a.slice("--port=".length));
    else if (a === "--retry-limit" && i + 1 < argv.length) retryLimit = Number(argv[i + 1]);
    else if (a.startsWith("--retry-limit=")) retryLimit = Number(a.slice("--retry-limit=".length));
    else if (a === "--home" && i + 1 < argv.length) home = argv[i + 1];
    else if (a.startsWith("--home=")) home = a.slice("--home=".length);
    else if (a === "--mock") mock = true;
  }
  if (!Number.isInteger(port) || port < 0 || port > 65535) port = 0;
  if (!Number.isInteger(retryLimit) || retryLimit < 1) retryLimit = DEFAULT_PORT_RETRY_LIMIT;
  return { port, retryLimit, home, mock };
}

/// Kill the Host and its whole process tree. On Windows `taskkill /T /F` covers
/// grandchildren; elsewhere SIGTERM then escalate to SIGKILL.
function killTree(pid) {
  if (process.platform === "win32") {
    try {
      spawn("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
      return;
    } catch {
      // fall through to child.kill()
    }
  }
  try {
    process.kill(pid, "SIGTERM");
  } catch {
    /* already gone */
  }
  setTimeout(() => {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      /* already gone */
    }
  }, 2000).unref();
}

function announceShutdown(child) {
  const shutdown = (why) => {
    process.stderr.write(`DSH_SHUTDOWN trigger=${why}\n`);
    killTree(child.pid);
    // taskkill is async; don't linger past this safety net.
    setTimeout(() => process.exit(0), 1500).unref();
  };
  // If the parent (Rust shell) dies or drops us, stdin closes -> kill the Host.
  // NOTE: on Windows the Rust shell must keep its piped-stdin write-end alive for
  // the whole sidecar lifetime (see sidecar.rs watcher thread), or we get a false
  // EOF here immediately after spawn and tear the Host down before it is ready.
  process.stdin.resume();
  process.stdin.on("end", () => shutdown("stdin-end"));
  process.stdin.on("close", () => shutdown("stdin-close"));
  process.on("SIGTERM", () => shutdown("sigterm"));
  process.on("SIGINT", () => shutdown("sigint"));
}

/// Launch the real upstream Host. `port` of 0 means the OS picks a free port; the
/// Host prints the actual one in its URL line, which we surface as DSH_READY.
function launchRealHost(port, home) {
  if (!existsSync(DSH_BIN)) {
    process.stderr.write(
      `DSH_ERROR runtime missing: ${DSH_BIN}\n` +
        "  install it with:  (cd host/runtime && npm install)\n",
    );
    process.exit(1);
  }
  mkdirSync(home, { recursive: true });

  const args = [DSH_BIN, "web", "--host", "127.0.0.1", "--no-open", "--port", String(port)];
  const child = spawn(process.execPath, args, {
    env: { ...process.env, DSH_HOME: home },
    stdio: ["pipe", "pipe", "pipe"],
  });

  let announced = false;
  let outBuf = "";
  let errBuf = "";

  const check = () => {
    if (announced) return;
    const hit = WEB_URL_RE.exec(outBuf + errBuf);
    if (hit) {
      announced = true;
      const p = Number(hit[1]);
      const token = hit[2];
      process.stdout.write(`DSH_READY ${p} ${token}\n`);
    }
  };

  // Forward the Host's own output for logs, and scan both streams for the URL
  // line (whichever stream it lands on). Buffering across chunk boundaries
  // handles the URL being split by the pipe.
  child.stdout.on("data", (chunk) => {
    outBuf += chunk.toString();
    process.stdout.write(chunk);
    check();
  });
  child.stderr.on("data", (chunk) => {
    errBuf += chunk.toString();
    process.stderr.write(chunk);
    check();
  });

  child.on("exit", (code, sig) => {
    if (!announced) {
      process.stderr.write(`DSH_ERROR host exited before ready (code=${code} sig=${sig})\n`);
      process.exit(code ?? 1);
    }
    // Host died after ready: the shell owns the window. Exit with code 0 so the
    // supervisor's watcher reaps us normally, but report the real exit reason on
    // stderr first so it lands in the shell logs and crash recovery can react.
    process.stderr.write(`dsh web exited code=${code} sig=${sig}\n`);
    process.exit(0);
  });

  announceShutdown(child);
}

/// Phase 1 placeholder server (used with `--mock` when the runtime isn't
/// installed). Kept so the shell still boots in CI/headless without the Host.
function launchMock(startPort, retryLimit) {
  const PLACEHOLDER_HTML = `<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8" /><title>DSH Tauri · mock host</title>
<style>
  body { margin:0; font-family:system-ui,-apple-system,"Segoe UI",sans-serif;
         background:#0f1115; color:#e6e6e6; display:grid; place-items:center; height:100vh; }
  .card { text-align:center; }
  h1 { font-size:1.4rem; font-weight:600; margin:0 0 .5rem; }
  p { color:#9aa3b2; margin:0; }
  code { background:#1c2027; padding:.15rem .4rem; border-radius:.3rem; color:#7dd3fc; }
</style></head>
<body><div class="card">
  <h1>DSH Tauri — sidecar 已就绪（mock）</h1>
  <p>真实 Host runtime 未安装时使用此占位页。</p>
  <p>端口：<code id="port">--</code></p>
</div><script>document.getElementById("port").textContent = location.port;</script></body>
</html>`;

  const tryListen = (startPort, retryLimit) => {
    const lastBoundary = startPort + retryLimit;
    return (async () => {
      for (let attempt = startPort; attempt < lastBoundary; attempt++) {
        try {
          return await new Promise((resolve, reject) => {
            const server = http.createServer((req, res) => {
              if (req.url === "/_dsh/health") {
                res.writeHead(200, { "content-type": "application/json" });
                res.end(JSON.stringify({ ok: true, port: attempt, service: "dsh-tauri-mock-host" }));
                return;
              }
              res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
              res.end(PLACEHOLDER_HTML);
            });
            server.once("error", reject);
            server.listen(attempt, "127.0.0.1", () => resolve(server));
          });
        } catch (err) {
          if (err.code !== "EADDRINUSE") throw err;
        }
      }
      throw new Error("端口范围耗尽");
    })();
  };

  tryListen(startPort, retryLimit).then(
    (server) => {
      const port = server.address().port;
      process.stdout.write(`DSH_READY ${port} mock-token\n`);
      announceShutdown({ pid: process.pid, exit: () => server.close(() => process.exit(0)) });
    },
    (err) => {
      process.stderr.write(`DSH_ERROR 端口 ${startPort} 起无法绑定（重试 ${retryLimit} 次）：${err.message}\n`);
      process.exit(1);
    },
  );
}

function main() {
  const { port, retryLimit, home, mock } = parseArgs(process.argv.slice(2));
  if (mock) {
    launchMock(port, retryLimit);
  } else {
    launchRealHost(port, home);
  }
}

main();
