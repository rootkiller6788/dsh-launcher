// Answer dependency questions about the runtime node_modules tree:
// which packages depend on a given set of targets, transitively.
// Usage: node scripts/query-deps.mjs <runtimeDir> <target1> [target2 ...]
// Prints, for each target, the top-level packages that (transitively) depend on it.

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const runtimeDir = process.argv[2] ?? join(import.meta.dirname, "..", "host", "runtime");
const targets = process.argv.slice(3);
if (!targets.length) {
  console.error("usage: node scripts/query-deps.mjs <runtimeDir> <target...>");
  process.exit(1);
}

const nm = join(runtimeDir, "node_modules");

function topLevel(nm) {
  const out = [];
  for (const name of readdirSync(nm)) {
    const dir = join(nm, name);
    if (name.startsWith("@")) {
      for (const sub of readdirSync(dir)) {
        const p = join(dir, sub);
        if (p.endsWith(".gitkeep") || p.endsWith(".cache")) continue;
        try {
          const st = require("node:fs").statSync(p);
          if (st.isDirectory()) out.push({ name: `${name}/${sub}`, dir: p });
        } catch {}
      }
    } else {
      out.push({ name, dir });
    }
  }
  return out;
}

function readPkg(dir) {
  try {
    return JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  } catch {
    return null;
  }
}

const items = topLevel(nm);
const byName = new Map(items.map((it) => [it.name, it]));
const pkg = new Map(items.map((it) => [it.name, readPkg(it.dir)]));

// For a target package name, find the actual dir it resolved to at top level.
const targetSet = new Set(targets);

// Build reverse dep map: dep -> [dependents]
const reverse = new Map();
for (const it of items) {
  const p = pkg.get(it.name);
  if (!p) continue;
  const deps = { ...p.dependencies, ...p.optionalDependencies, ...p.peerDependencies };
  for (const d of Object.keys(deps)) {
    if (!reverse.has(d)) reverse.set(d, new Set());
    reverse.get(d).add(it.name);
  }
}

// BFS from each target upward through reverse edges, reporting top-level packages.
function upwardClosure(target) {
  const seen = new Set();
  const queue = [target];
  while (queue.length) {
    const cur = queue.shift();
    for (const dep of reverse.get(cur) ?? []) {
      if (!seen.has(dep)) {
        seen.add(dep);
        queue.push(dep);
      }
    }
  }
  return [...seen];
}

for (const target of targetSet) {
  const via = upwardClosure(target);
  const chain = via
    .filter((n) => byName.has(n))
    .map((n) => {
      const p = pkg.get(n);
      return p?.description ? `${n}  (${p.description})` : n;
    });
  console.log(`\n== ${target} is depended on by ${via.length} top-level packages ==`);
  for (const c of chain) console.log(`   ${c}`);
}
