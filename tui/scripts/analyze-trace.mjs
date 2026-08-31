// Analyze a module-load trace (from scripts/trace-loader.mjs) against the
// installed node_modules tree:
//   - which top-level packages were actually loaded during the traced run
//   - which top-level packages were NOT loaded (prunable candidates), by size
//
// Usage: node scripts/analyze-trace.mjs <traceFile> [runtimeDir]

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const traceFile = process.argv[2];
const runtimeDir = process.argv[3] ?? join(import.meta.dirname, "..", "host", "runtime");
const nm = join(runtimeDir, "node_modules");
const MB = 1024 * 1024;

// --- full top-level package list with sizes ---
function topLevel(nm) {
  const out = [];
  for (const name of readdirSync(nm)) {
    const dir = join(nm, name);
    if (name.startsWith("@")) {
      for (const sub of readdirSync(dir)) {
        const p = join(dir, sub);
        let st;
        try {
          st = statSync(p);
        } catch {
          continue;
        }
        if (st.isDirectory()) out.push({ name: `${name}/${sub}`, dir: p });
      }
    } else {
      let st;
      try {
        st = statSync(dir);
      } catch {
        continue;
      }
      if (st.isDirectory()) out.push({ name, dir });
    }
  }
  return out;
}
function dirSize(dir) {
  let total = 0;
  const stack = [dir];
  while (stack.length) {
    const cur = stack.pop();
    let entries;
    try {
      entries = readdirSync(cur, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const p = join(cur, e.name);
      if (e.isDirectory()) stack.push(p);
      else {
        try {
          total += statSync(p).size;
        } catch {}
      }
    }
  }
  return total;
}
const all = topLevel(nm).map((it) => ({ name: it.name, size: dirSize(it.dir) }));
const byName = new Map(all.map((it) => [it.name, it]));

// --- loaded set from trace ---
const loaded = new Set();
const fileRE = /\/node_modules\/((?:@[^/]+\/)?[^/]+)/;
for (const line of readFileSync(traceFile, "utf8").split("\n")) {
  const m = fileRE.exec(line);
  if (m) loaded.add(m[1]);
}

const notLoaded = all.filter((it) => !loaded.has(it.name)).sort((a, b) => b.size - a.size);
const notLoadedBytes = notLoaded.reduce((s, it) => s + it.size, 0);
const totalBytes = all.reduce((s, it) => s + it.size, 0);

console.log(`full: ${all.length} top-level packages, ${(totalBytes / MB).toFixed(0)} MB`);
console.log(`loaded: ${loaded.size} packages, ${((totalBytes - notLoadedBytes) / MB).toFixed(0)} MB`);
console.log(`NOT loaded: ${notLoaded.length} packages, ${(notLoadedBytes / MB).toFixed(0)} MB\n`);

console.log("=== NOT-loaded, by size (prunable candidates) ===");
for (const it of notLoaded.slice(0, 60)) {
  console.log(`${(it.size / MB).toFixed(1).padStart(8)} MB  ${it.name}`);
}
console.log("...");
