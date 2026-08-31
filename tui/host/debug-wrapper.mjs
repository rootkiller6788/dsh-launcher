// Debug: spawn host/index.js with a kept-open stdin pipe (mimics Rust's
// Stdio::piped() where the parent never closes stdin) and watch the result.
import { spawn } from "node:child_process";

const child = spawn(process.execPath, ["index.js", "--port", "0"], {
  stdio: ["pipe", "pipe", "pipe"],
});

child.stdout.on("data", (c) => process.stdout.write("[wrapper out] " + c.toString()));
child.stderr.on("data", (c) => process.stdout.write("[wrapper err] " + c.toString()));
child.on("exit", (code, sig) => {
  process.stdout.write(`[wrapper exit] code=${code} sig=${sig}\n`);
  process.exit(0);
});
child.on("error", (e) => {
  process.stdout.write(`[wrapper spawn error] ${e}\n`);
  process.exit(1);
});

// Deliberately never close child.stdin — the pipe write end stays open, exactly
// like the Rust side keeps its ChildStdin alive.
setTimeout(() => {
  process.stdout.write("[debug] 12s elapsed, killing wrapper\n");
  child.kill();
}, 12000).unref();
