// End-to-end smoke of the real-Host sidecar wrapper:
//   node host/index.js  ->  spawns real `dsh web`  ->  prints DSH_READY <port> <token>
//   then the token URL must serve the real DSH UI (HTTP 200, not 401),
//   and closing the wrapper's stdin must tear down the whole dsh tree.
// Usage: node scripts/smoke-sidecar-wrapper.mjs

import { spawn } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

const here = fileURLToPath(new URL(".", import.meta.url));
const wrapper = join(here, "..", "host", "index.js");

const child = spawn(process.execPath, [wrapper, "--port", "0"], {
  stdio: ["pipe", "pipe", "pipe"],
});
// Keep stdin open until we decide to shut down.
child.stdin.resume();

const rl = createInterface({ input: child.stdout });
const stderrLines = [];
child.stderr.on("data", (d) => stderrLines.push(d.toString()));

const deadline = Date.now() + 60_000;
const ready = await new Promise((resolve) => {
  rl.on("line", (line) => {
    console.log(`[wrapper] ${line}`);
    const m = /^DSH_READY (\d+) (\S+)$/.exec(line.trim());
    if (m) resolve({ port: Number(m[1]), token: m[2] });
  });
  setTimeout(() => resolve(null), deadline - Date.now()).unref();
});

if (!ready) {
  console.error("TIMEOUT waiting for DSH_READY. stderr so far:\n" + stderrLines.join(""));
  child.kill();
  process.exit(1);
}
console.log(`\nREADY on port ${ready.port}, token ${ready.token.slice(0, 8)}…`);

// Bare / must be 401 (browser-trust token required)…
const bare = await fetch(`http://127.0.0.1:${ready.port}/`).catch((e) => ({ status: `fetch-error: ${e.message}` }));
console.log(`bare  /                     -> HTTP ${typeof bare.status === "number" ? bare.status : bare.status}`);
// …while the token URL must exchange the launch token for an authority-bound
// session cookie (303 -> / + Set-Cookie), which is what a real webview's cookie
// jar does automatically. Node's fetch has no cookie jar, so replay it manually:
const exch = await fetch(`http://127.0.0.1:${ready.port}/?token=${ready.token}`, {
  redirect: "manual",
  signal: AbortSignal.timeout(10_000),
});
const setCookie = exch.headers.get("set-cookie");
console.log(`token /?token=<t>          -> HTTP ${exch.status}${exch.status === 303 ? " (redirect)" : ""} set-cookie: ${setCookie ? setCookie.split(";")[0] : "(none)"}`);
const cookieOk = exch.status === 303 && setCookie !== null;
const cookie = cookieOk ? setCookie.split(";")[0] : "";
// Now the follow-up GET / with the cookie must serve the real DSH SPA.
const authed = cookieOk
  ? await fetch(`http://127.0.0.1:${ready.port}/`, {
      headers: { cookie },
      signal: AbortSignal.timeout(10_000),
    }).catch((e) => ({ status: `fetch-error: ${e.message}`, text: "" }))
  : { status: "(skipped)", text: "" };
const body = authed.text ? await authed.text() : "";
console.log(`cookie GET /               -> HTTP ${typeof authed.status === "number" ? authed.status : authed.status}`);
console.log("  first 2 lines of body:", body.slice(0, 300).split("\n").slice(0, 2).map((s) => s.trim()).filter(Boolean).join(" | "));

const ok = typeof bare.status === "number" && bare.status === 401 && cookieOk && typeof authed.status === "number" && authed.status === 200;
console.log(`\nTOKEN GATING ${ok ? "OK" : "UNEXPECTED"} (401 bare / 303 exchange / 200 SPA with cookie)`);

// Close stdin -> wrapper must kill the dsh tree and exit.
child.stdin.end();
const exit = await new Promise((resolve) => child.on("exit", (c, s) => resolve({ c, s })));
console.log(`wrapper exited code=${exit.c} sig=${exit.s}`);

// The web server must be gone now.
const probe = await fetch(`http://127.0.0.1:${ready.port}/`).catch((e) => ({ gone: true, msg: e.message }));
console.log(`post-exit probe -> ${probe.gone ? "port closed (no residual host)" : "STILL LISTENING: " + JSON.stringify(probe)}`);

const clean = exit.c === 0 && probe.gone === true;
console.log(`\nCLEANUP ${clean ? "OK" : "FAILED"}`);
process.exit(ok && clean ? 0 : 1);
