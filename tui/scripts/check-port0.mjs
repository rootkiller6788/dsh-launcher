// Quick check: does `dsh web --port 0` print the OS-assigned port in its URL line?
import { spawn } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const runtime = join(here, "..", "host", "runtime");
const dshBin = join(runtime, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
const home = join(runtime, ".smoke-home");

rmSync(home, { recursive: true, force: true });
mkdirSync(home, { recursive: true });

const child = spawn(process.execPath, [dshBin, "web", "--host", "127.0.0.1", "--no-open", "--port", "0"], {
  env: { ...process.env, DSH_HOME: home },
  stdio: ["pipe", "pipe", "pipe"],
});

let out = "";
child.stdout.on("data", (d) => { out += d.toString(); });
child.stderr.on("data", (d) => { out += d.toString(); });

setTimeout(() => {
  console.log("--- captured output (first 500 chars) ---");
  console.log(out.slice(0, 500));
  child.kill();
  process.exit(0);
}, 15_000);
