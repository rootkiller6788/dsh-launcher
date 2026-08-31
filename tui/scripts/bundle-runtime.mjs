// Stage the assets that `tauri build` bundles via `bundle.resources`:
//
//   src-tauri/bundle-assets/node/node.exe             official Node binary (matches the
//                                                     system node that the dev runtime is
//                                                     built against — read from the running
//                                                     `node --version`, e.g. v24.13.1)
//   src-tauri/bundle-assets/host/index.js             the sidecar wrapper
//   src-tauri/bundle-assets/host/runtime/node_modules the DSH host runtime (production tree)
//
// Deliberately NON-destructive: it never touches `host/runtime` in place; user state
// (`host/runtime/.dsh-home`) is never staged. Idempotent — run with `--force` to redo
// the copy; the prune step below always runs, so a previously-staged (unpruned) tree
// gets upgraded to pruned on the next invocation without a full re-copy.
//
// Deep prune: the generated `host/runtime/package.json` lists every vendored
// @deepseek-ai/* package as a regular `dependency`, so `npm prune --omit=dev` is a
// no-op and the dev tree (1,038 MB) is dominated by two subagent driver SDKs
// (@openai/codex-win32-x64 373 MB + @anthropic-ai/claude-agent-sdk-win32-x64 322 MB,
// used only by dsh-subagent-codex / dsh-subagent-claude-code) plus test/demo packages.
// After pruning the staged tree drops to ~340 MB. The removal list below is
// data-driven (scripts/prune-check.mjs): every entry is referenced by zero *kept*
// packages through runtime deps. Kept deliberately: @anthropic-ai/sdk (pi-ai provider),
// @modelcontextprotocol/sdk (dsh-mcp-client + @google/genai), dsh-loader-smoke and
// dsh-agent-spine-demo (runtime deps of dsh-session-snapshot / dsh-sdk-minimal), and
// the dev tooling (typescript/vite/vitest/...) that kept packages list as runtime deps.

import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..");
const hostDir = join(repoRoot, "host");
const runtimeDir = join(hostDir, "runtime");
const assetsDir = join(repoRoot, "src-tauri", "bundle-assets");
const assetsHost = join(assetsDir, "host");
const assetsNode = join(assetsDir, "node");
const nodeExe = join(assetsNode, "node.exe");
const force = process.argv.includes("--force");

const NODE_VERSION = process.versions.node; // e.g. "24.13.1" — matches dev runtime ABI.
const NODE_ZIP_URL = `https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-win-x64.zip`;

const MB = 1024 * 1024;

// Packages removed from the *staged* runtime copy. See the header comment for the
// data-driven justification. All are PRUNE-SAFE: no kept package lists them in
// dependencies/optionalDependencies/peerDependencies.
const PRUNE = [
  // Subagent drivers (user chose the aggressive profile): lose the Codex and
  // Claude-Code subagent features, keep the in-process/spawn subagents and everything else.
  "@deepseek-ai/dsh-subagent-codex",
  "@deepseek-ai/dsh-subagent-claude-code",
  // Exclusive native SDKs dragged in by the two drivers above (~697 MB combined).
  "@openai/codex",
  "@openai/codex-win32-x64",
  "@anthropic-ai/claude-agent-sdk",
  "@anthropic-ai/claude-agent-sdk-win32-x64",
  // Test/demo scaffolding (never loaded at boot+UI-serve; zero runtime dependents).
  "@deepseek-ai/dsh-agent-loop-testkit",
  "@deepseek-ai/dsh-llm-replay",
  "@deepseek-ai/dsh-llm-mock-server",
  "@deepseek-ai/dsh-client-test-runtime",
  "@deepseek-ai/dsh-typert-generator",
];
// Recursive byte total of a directory (skips unreadable entries silently).
const dirSize = (dir) => {
  if (!existsSync(dir)) return 0;
  let total = 0;
  const stack = [dir];
  while (stack.length) {
    const cur = stack.pop();
    let names;
    try {
      names = readdirSync(cur);
    } catch {
      continue;
    }
    for (const name of names) {
      const full = join(cur, name);
      let st;
      try {
        st = statSync(full);
      } catch {
        continue;
      }
      if (st.isDirectory()) stack.push(full);
      else if (st.isFile()) total += st.size;
    }
  }
  return total;
};

// --- node.exe ----------------------------------------------------------------
async function stageNode() {
  if (existsSync(nodeExe) && !force) {
    console.log(`[node] ${nodeExe} already staged; use --force to redo`);
    return;
  }
  mkdirSync(assetsNode, { recursive: true });
  const zipPath = join(assetsNode, `node-v${NODE_VERSION}-win-x64.zip`);
  if (!existsSync(zipPath)) {
    console.log(`[node] downloading ${NODE_ZIP_URL}`);
    const res = await fetch(NODE_ZIP_URL);
    if (!res.ok) throw new Error(`download failed: HTTP ${res.status}`);
    const buf = Buffer.from(await res.arrayBuffer());
    writeFileSync(zipPath, buf);
    console.log(`[node] downloaded ${(buf.length / MB).toFixed(1)} MB`);
  }
  const extractDir = join(assetsNode, "extract");
  rmSync(extractDir, { recursive: true, force: true });
  mkdirSync(extractDir, { recursive: true });
  console.log("[node] extracting node.exe (PowerShell Expand-Archive)…");
  execFileSync(
    "powershell",
    [
      "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
      `Expand-Archive -LiteralPath '${zipPath}' -DestinationPath '${extractDir}' -Force`,
    ],
    { stdio: "inherit" },
  );
  const extracted = join(extractDir, `node-v${NODE_VERSION}-win-x64`, "node.exe");
  if (!existsSync(extracted)) throw new Error(`node.exe not found after extraction at ${extracted}`);
  rmSync(nodeExe, { force: true });
  mkdirSync(assetsNode, { recursive: true });
  // Move, don't copy (drop the .zip + extract dir afterwards).
  cpSync(extracted, nodeExe);
  rmSync(join(assetsNode, "extract"), { recursive: true, force: true });
  rmSync(zipPath, { force: true });
  console.log(`[node] staged ${nodeExe} (${(statSync(nodeExe).size / MB).toFixed(1)} MB)`);
}

// --- host runtime ------------------------------------------------------------
function stageHost() {
  mkdirSync(assetsHost, { recursive: true });

  // index.js sidecar wrapper.
  cpSync(join(hostDir, "index.js"), join(assetsHost, "index.js"));

  // package.json + lockfile so npm tooling works against the staged tree later.
  mkdirSync(join(assetsHost, "runtime"), { recursive: true });
  for (const f of ["package.json", "package-lock.json"]) {
    const src = join(runtimeDir, f);
    if (existsSync(src)) cpSync(src, join(assetsHost, "runtime", f));
  }

  // node_modules — full production tree. User state (`runtime/.dsh-home`) is not
  // part of node_modules and is therefore never staged.
  const src = join(runtimeDir, "node_modules");
  const dst = join(assetsHost, "runtime", "node_modules");
  if (existsSync(dst) && !force) {
    console.log("[host] runtime already staged; use --force to redo the copy");
  } else {
    console.log("[host] copying node_modules (this is the big one)…");
    rmSync(dst, { recursive: true, force: true });
    cpSync(src, dst, { recursive: true, verbatimSymlinks: true });
    console.log("[host] staged runtime");
  }

  // Always prune, even on an already-staged tree — so an existing unpruned
  // staging is upgraded without a full re-copy. Idempotent.
  pruneRuntime(dst);
}

// Remove the PRUNE set from the staged copy, then drop any scope directories the
// removal left empty (e.g. @openai once both codex packages are gone). Never
// touches the dev tree.
function pruneRuntime(nm) {
  let removedBytes = 0;
  for (const name of PRUNE) {
    const dir = join(nm, name);
    if (!existsSync(dir)) continue;
    removedBytes += dirSize(dir);
    rmSync(dir, { recursive: true, force: true });
    console.log(`[prune] removed ${name}`);
  }
  if (existsSync(nm)) {
    for (const scope of readdirSync(nm)) {
      const scopeDir = join(nm, scope);
      if (!scope.startsWith("@") || !statSync(scopeDir).isDirectory()) continue;
      const left = readdirSync(scopeDir).filter((e) => e !== ".gitkeep" && e !== ".cache");
      if (left.length === 0) {
        rmSync(scopeDir, { recursive: true, force: true });
        console.log(`[prune] removed empty scope ${scope}`);
      }
    }
  }

  // Keep the staged package.json self-consistent: drop the pruned packages from
  // its dependency declarations so nothing that walks the manifest (npm tooling,
  // a future loader) trips over a declared-but-absent package.
  const pkgFile = join(dirname(nm), "package.json");
  if (existsSync(pkgFile)) {
    try {
      const pj = JSON.parse(readFileSync(pkgFile, "utf8"));
      let changed = false;
      for (const section of ["dependencies", "optionalDependencies", "devDependencies", "peerDependencies"]) {
        if (!pj[section]) continue;
        for (const name of PRUNE) {
          if (pj[section][name]) {
            delete pj[section][name];
            changed = true;
          }
        }
      }
      if (changed) writeFileSync(pkgFile, JSON.stringify(pj, null, 2) + "\n");
    } catch {
      /* keep the unmodified manifest if it can't be parsed */
    }
  }

  console.log(`[prune] freed ${(removedBytes / MB).toFixed(1)} MB`);
}

async function main() {
  await stageNode();
  stageHost();
  const nodeMb = existsSync(nodeExe) ? (statSync(nodeExe).size / MB).toFixed(1) : "?";
  const hostMbFixed = (dirSize(join(assetsHost, "runtime", "node_modules")) / MB).toFixed(0);
  console.log("---");
  console.log(`bundle-assets staged under ${assetsDir}`);
  console.log(`  node.exe: ${nodeMb} MB`);
  console.log(`  host runtime: ${hostMbFixed} MB`);
  console.log("Run `npm run tauri build` to produce the installer.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
