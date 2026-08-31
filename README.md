# dsh-launcher

DSHL — A Windows & macOS desktop launcher for DeepSeek-Harness that simplifies runtime installation, isolated instances, provider configuration, plugin management, and one-click launching—no terminal, Node.js, or source setup required.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%2B-0078D6)](#)
![Tauri 2](https://img.shields.io/badge/Tauri-2-ffc131)
![React 19](https://img.shields.io/badge/React-19-61dafb)
![Rust](https://img.shields.io/badge/Rust-1.82%2B-dea584)

---

## What it does

dsh-launcher is a desktop launcher for AI harnesses that manages the entire lifecycle of your DSH environments—from setup and configuration to plugins, runtimes, and launching.

| | |
| --- | --- |
| **Runtimes** | Versioned DSH installs with a bundled Node 22 — a fresh Windows machine works out of the box. |
| **Instances** | Multiple isolated DSH environments (each its own `$DSH_HOME`), create / rename / clone / delete / switch. |
| **Providers** | API key in Windows Credential Manager, never on disk in plaintext. |
| **Plugin Market** | Registry browse + smart (LLM re-ranked) search + install / uninstall / hot enable-disable. |
| **Launch** | One button → `dsh web` boots → DSH renders in a launcher-owned window, logs streamed live. |

## Features

- **Out-of-the-box runtime** — Node 22 is bundled into the installer; DSH is resolved through a 4-layer chain (settings override → bundled → managed `runtimes/<ver>/` → PATH). No Node, no pnpm, no source checkout required.
- **Managed runtimes** — install / switch / remove / verify DSH versions side by side; each instance pins its own version.
- **Multi-instance isolation** — every instance gets an isolated `$DSH_HOME`; plugins and config never leak between instances.
- **Smart Plugin Market** — port of `dsh-market` + `smart-plugin-market` in Rust: registry fetch (China npm-mirror fallback), smart search with `Result ⊆ Registry` validation, hot enable/disable via the `cordis.patch.yml` layer (survives `dsh plugin` reconciliation).
- **Diagnostics** — bundle-stack + duplicate-entry + orphan-patch-target + load-order analysis with fix suggestions.
- **DSH in-app window** — DSH renders inside the launcher (Option B / dsh-tauri port), not a browser tab; closing the window stops the harness.
- **Bidirectional theme sync** — light/dark/system, one lamp: toggling in the launcher or in DSH keeps both in step.
- **Process-tree hardening** — Windows Job Object (`KILL_ON_JOB_CLOSE`) + recursive `taskkill /T /F` fallback + a pre-launch zombie sweep. 10 consecutive stop/start cycles leave zero survivors.
- **Launch history** — SQLite-backed sessions with start/stop timestamps and crash/exit status.

## Install

### Download (recommended)

Grab the latest `dsh-launcher_<version>_x64-setup.exe` from [Releases](../../releases), run it, done. The installer ships a bundled Node runtime, so there's nothing else to install.

> ⚠️ Releases are currently **unsigned**. Windows SmartScreen may warn on first run — click "More info → Run anyway". (Code signing is on the [roadmap](#roadmap), `P5`.)

### Build from source

Requirements: Node ≥ 22, pnpm ≥ 10, Rust (MSVC toolchain), WebView2 (built into Win 11).

```bash
pnpm install                 # frontend deps
cargo build --workspace      # compile all Rust crates
pnpm build                   # tauri build → NSIS setup.exe
# artifact: apps/desktop/src-tauri/target/release/bundle/nsis/*-setup.exe
```

## Quick start

1. **Install** and launch dsh-launcher.
2. **Settings → Provider** — paste a DeepSeek API key (stored in Windows Credential Manager).
3. **Home** — pick an instance, click **Launch**.

DSH boots in its own window; the Activity pane streams stdout/stderr live. Stop from the same button or by closing the window.

## Repository layout

```
dsh-launcher/
├── apps/desktop/               Tauri 2 app
│   ├── src/                    React 19 + TS frontend (Vite, Tailwind v4, Zustand)
│   └── src-tauri/              Rust shell: commands/, state/, tauri.conf.json, vendor/node
├── crates/
│   ├── launcher-core/          framework-agnostic core: paths/settings/instance/provider/process/runtime/history/market
│   └── dsh-adapter/            DSH-specific adapter (RuntimeAdapter impl): runtimes/theme/diagnostics
├── tui/                        dsh-tauri reference mirror (Option B mechanism)
├── scripts/                    dev helpers (icon generation, …)
└── docs: README.md · plan.md · architecture.md · TODO.md · CONTRIBUTING.md · LICENSE
```

**Language boundary (by design):** TypeScript owns the UI; Rust owns the system (process, fs, network, secrets, SQLite). All system actions go through typed Tauri IPC — see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Data directory

Everything lives under `%LOCALAPPDATA%/AIHarnessLauncher/`:

```
AIHarnessLauncher/
├── settings.json             app settings (DSH path override, theme, last instance)
├── providers.json            provider metadata (API key is in Credential Manager, never here)
├── launcher.db               SQLite launch history
├── runtimes/                 managed DSH versions + bundled node
├── instances/<id>/           instance.json + workspace/ (= that instance's $DSH_HOME)
├── cache/                    registry + download cache
└── logs/launcher.log         app log (+ crash-*.txt on panic)
```

## Environment variables

| Variable | Purpose |
| --- | --- |
| `AHL_HOME` | Override the data root (default `%LOCALAPPDATA%/AIHarnessLauncher`; dev/testing) |
| `DSH_CLI_BIN` | Override the DSH CLI entry (`…/apps/cli/lib/bin.js`) |

## Architecture

See [`architecture.md`](architecture.md) for the full product architecture and [`plan.md`](plan.md) for the phased plan (Phase 0–7) and the productization plan (P0–P6) with acceptance gates.

## Roadmap

Tracked in [`TODO.md`](TODO.md). Milestones:

- **v0.4** — out-of-the-box on a clean machine (✅ done)
- **v0.6** — no-scar stop/start + green CI (in progress)
- **v0.7** — signed installer + in-app auto-update
- **v1.0** — signed + auto-update + out-of-the-box + CI-guarded stable

## Contributing

Pull requests welcome — read [`CONTRIBUTING.md`](CONTRIBUTING.md) first (branch/commit conventions, PR flow, code & test standards).

## License

[MIT](LICENSE) © 2026 dsh-launcher contributors.
