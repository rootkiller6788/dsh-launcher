// Temporary: report which candidate prune targets are referenced by KEPT
// packages via runtime deps (dependencies/optional/peer). devDeps refs are
// ignored — they don't exist in a pruned consumer tree.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const runtimeDir = process.argv[2] ?? join(import.meta.dirname, "..", "host", "runtime");
const nm = join(runtimeDir, "node_modules");

const targets = [
  "@deepseek-ai/dsh-agent-loop-testkit",
  "@deepseek-ai/dsh-loader-smoke",
  "@deepseek-ai/dsh-llm-replay",
  "@deepseek-ai/dsh-llm-mock-server",
  "@deepseek-ai/dsh-client-test-runtime",
  "@deepseek-ai/dsh-agent-spine-demo",
  "@deepseek-ai/dsh-typert-generator",
  "@deepseek-ai/dsh-subagent-codex",
  "@deepseek-ai/dsh-subagent-claude-code",
  "@openai/codex",
  "@openai/codex-win32-x64",
  "@anthropic-ai/claude-agent-sdk",
  "@anthropic-ai/claude-agent-sdk-win32-x64",
];

const out = [];
for (const n of readdirSync(nm)) {
  const dir = join(nm, n);
  if (n.startsWith("@")) {
    for (const s of readdirSync(dir)) out.push(join(dir, s));
  } else {
    out.push(dir);
  }
}
const pkgs = out.filter((d) => !d.endsWith(".gitkeep") && !d.endsWith(".cache"));

const declaredBy = {};
for (const d of pkgs) {
  const name = d.slice(nm.length + 1).replace(/\\/g, "/");
  if (targets.includes(name)) continue; // don't count the candidate itself
  let pj;
  try {
    pj = JSON.parse(readFileSync(join(d, "package.json"), "utf8"));
  } catch {
    continue;
  }
  const rt = { ...pj.dependencies, ...pj.optionalDependencies, ...pj.peerDependencies };
  for (const t of targets) {
    if (rt[t]) (declaredBy[t] ??= []).push(name);
  }
}

for (const t of targets) {
  const refs = declaredBy[t] || [];
  const verdict = refs.length ? "RUNTIME-REF (KEEP)" : "PRUNE-SAFE       ";
  console.log(verdict + "  " + t.padEnd(40) + "-> " + (refs.join(", ") || "(no runtime ref)"));
}
