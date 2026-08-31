// Debug: mimic host/index.js launchRealHost's spawn and capture dsh's real output.
import { spawn } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const DSH_BIN = join(HERE, "runtime", "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
const HOME = join(HERE, "runtime", ".dsh-home");

const child = spawn(process.execPath, [DSH_BIN, "web", "--host", "127.0.0.1", "--no-open", "--port", "0"], {
  env: { ...process.env, DSH_HOME: HOME },
  stdio: ["pipe", "pipe", "pipe"],
});

child.stdout.on("data", (c) => process.stdout.write("[dsh stdout] " + c.toString()));
child.stderr.on("data", (c) => process.stdout.write("[dsh stderr] " + c.toString()));
child.on("exit", (code, sig) => {
  process.stdout.write(`[dsh exit] code=${code} sig=${sig}\n`);
  process.exit(0);
});
child.on("error", (e) => {
  process.stdout.write(`[dsh spawn error] ${e}\n`);
  process.exit(1);
});
// Keep stdin open like the real wrapper does (never close it).
setTimeout(() => {
  process.stdout.write("[debug] 12s elapsed, killing child\n");
  child.kill();
}, 12000).unref();
