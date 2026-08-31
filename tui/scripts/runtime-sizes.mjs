// Report the biggest consumers of disk under a node_modules tree, so the
// deep-prune decision is data-driven. Lists top-level packages (including the
// @deepseek-ai scope) by recursive size.
//
// Usage: node scripts/runtime-sizes.mjs [runtimeDir] [topN]

import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const runtimeDir = process.argv[2] ?? join(import.meta.dirname, "..", "host", "runtime");
const topN = Number(process.argv[3] ?? 30);
const nm = join(runtimeDir, "node_modules");

const MB = 1024 * 1024;

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
        } catch {
          /* ignore */
        }
      }
    }
  }
  return total;
}

function topLevel(nm) {
  const out = [];
  for (const name of readdirSync(nm)) {
    const dir = join(nm, name);
    if (name.startsWith("@")) {
      // scoped: recurse one level, report each scoped package
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

const items = topLevel(nm);
const sized = items.map((it) => ({ name: it.name, size: dirSize(it.dir) })).sort((a, b) => b.size - a.size);
const grandTotal = sized.reduce((s, i) => s + i.size, 0);
console.log(`node_modules total: ${(grandTotal / MB).toFixed(0)} MB across ${sized.length} top-level packages\n`);
console.log(`top ${topN} by size:`);
for (const it of sized.slice(0, topN)) {
  console.log(`${(it.size / MB).toFixed(1).padStart(8)} MB  ${it.name}`);
}
