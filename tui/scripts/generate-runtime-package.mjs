// Generate host/runtime/package.json from the pinned vendored-runtime manifest.
//
// The vendored runtime in dsh-desktop ships every @deepseek-ai/* package as a
// prebuilt tgz (official build profile, sha256-verified). We reference each one
// directly so npm assembles the exact pinned runtime without building the
// upstream source; non-@deepseek-ai deps (cordis, schemastery, ...) resolve from
// the public registry.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = join(here, "..");

// Path to the pinned manifest in the sibling dsh-desktop repo.
const manifestPath = join(
  repoRoot,
  "..",
  "dsh-desktop",
  "vendor",
  "dsh-runtime",
  "0.1.2-alpha.1",
  "manifest.json",
);
const vendorDir = join(manifestPath, "..");

const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
if (manifest.formatVersion !== 1) {
  throw new Error(`unsupported vendored-runtime manifest format: ${manifest.formatVersion}`);
}

const dependencies = {};
for (const pkg of manifest.packages) {
  dependencies[pkg.name] = `file:${join(vendorDir, pkg.filename).replace(/\\/g, "/")}`;
}

const runtimeDir = join(repoRoot, "host", "runtime");
const pkgJson = {
  name: "dsh-tauri-runtime",
  version: manifest.version,
  private: true,
  description: `Pinned DeepSeek Harness runtime (${manifest.commit.slice(0, 12)}, ${manifest.buildProfile} build) assembled from the dsh-desktop vendored packages.`,
  dependencies,
};

writeFileSync(
  join(runtimeDir, "package.json"),
  `${JSON.stringify(pkgJson, null, 2)}\n`,
);

const count = manifest.packages.length;
console.log(`wrote ${join(runtimeDir, "package.json")} with ${count} pinned @deepseek-ai/* dependencies`);
