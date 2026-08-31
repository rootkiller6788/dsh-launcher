// Smoke-test launching the REAL DSH Host (web profile) from the vendored runtime.
// Mirrors what the sidecar will do: spawn, wait for HTTP readiness, report, then
// kill. Usage: node scripts/smoke-real-host.mjs [--port 43120]

import { spawn } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const runtime = join(here, "..", "host", "runtime");
const dshBin = join(runtime, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");

const argPort = process.argv.indexOf("--port");
const port = argPort >= 0 ? Number(process.argv[argPort + 1]) : 43120;
const home = join(runtime, ".smoke-home");

rmSync(home, { recursive: true, force: true });
mkdirSync(home, { recursive: true });

const child = spawn(process.execPath, [dshBin, "web", "--host", "127.0.0.1", "--no-open", "--port", String(port)], {
  env: { ...process.env, DSH_HOME: home },
  stdio: ["pipe", "pipe", "pipe"],
});

let log = "";
child.stdout.on("data", (d) => { log += d.toString(); process.stdout.write(`[out] ${d}`); });
child.stderr.on("data", (d) => { log += d.toString(); process.stderr.write(`[err] ${d}`); });

const deadline = Date.now() + 45_000;
const poll = async () => {
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/`, { signal: AbortSignal.timeout(1500) });
      console.log(`\nREADY: HTTP ${res.status} on port ${port}`);
      console.log("--- last 15 log lines ---");
      console.log(log.split("\n").slice(-15).join("\n"));
      console.log("--- first 6 lines of body ---");
      console.log((await res.text()).split("\n").slice(0, 6).join("\n"));
      process.exit(0);
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  console.log("TIMEOUT: web server never became ready. Last log:");
  console.log(log.split("\n").slice(-25).join("\n"));
  process.exit(1);
};

child.on("exit", (code, sig) => {
  console.log(`\nchild exited code=${code} sig=${sig}`);
  console.log("--- last 25 log lines ---");
  console.log(log.split("\n").slice(-25).join("\n"));
  process.exit(code ?? 1);
});

poll();
