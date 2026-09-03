# DSH-first capability boundaries

Launcher treats DSH as the runtime trunk. Launcher can download, classify,
validate, cache and diagnose resources, but it should not become the authority
for whether DSH can see or use them.

## Capability table

| Resource | Install entrypoint | True state source | Launcher cache source | Launcher scan role |
| --- | --- | --- | --- | --- |
| Plugins | DSH native `dsh plugin add/remove/toggle`; live `pluginInventory.list` after launch | DSH Inventory | DSH Inventory snapshot plus Market metadata | Enrich rows with category, icon, source, install job and diagnostics metadata. Do not decide enabled/existing state without DSH. |
| Skins / UI | Same as plugins when packaged as DSH plugins or bundles | DSH Inventory | DSH Inventory snapshot, instance skin classification, Market metadata | Classify theme-like plugin rows into Skins/UI and preserve Market origin. DSH still owns install and activation. |
| Skills | Launcher downloads `SKILL.md` into the instance `skills/` folder that DSH discovers | DSH workspace files discovered by DSH | Instance manifest plus Market metadata | Validate/download/index skill files and keep export/import metadata. DSH owns actual runtime discovery and use. |
| MCP servers | Launcher writes DSH-recognized MCP client config or patch rows for the instance | DSH workspace config discovered by DSH | Instance manifest plus Market metadata | Validate catalog config, write compatible MCP rows, and track install source. DSH owns runtime mounting/discovery. |
| Bundles | Launcher expands the bundle into plugin/theme/skill/mcp leaf installs | Per leaf resource | Market metadata and per-leaf install results | Track the import plan and job progress only. Bundles are not runtime inventory objects. |

## Rules

1. Market install must first write through DSH or a DSH-recognized workspace
   location, then Launcher records metadata and refreshes cache.
2. `library-inventory.json` is a snapshot cache. It is allowed to make pages
   open fast, but it is not allowed to overrule DSH Inventory.
3. Startup must not deep-scan `node_modules`, run diagnostics, or check updates.
   It should show Workspace first, then run lightweight background sync.
4. Library may show cached rows immediately, then reconcile with DSH when the
   instance is running.
5. Instances and Overview should read summary snapshots, not full plugin rows.

## Snapshot cache schema

`library-inventory.json` is a cache, not the database of truth. Version 3 uses
field names that make that explicit:

```json
{
  "schemaVersion": 3,
  "instanceId": "default",
  "updatedAt": 123456789,
  "dshInventory": [],
  "launcherMetadata": {},
  "skills": [],
  "mcp": [],
  "skins": [],
  "installSources": {}
}
```

Read behavior:

1. Opening Library reads this file only and renders immediately.
2. Background launch reconciliation updates `dshInventory` from DSH when the
   instance is running.
3. Market installs update the resource first, then write `launcherMetadata` and
   `installSources`, then reconcile the snapshot.
4. Workspace/Manage mode switches must not scan profiles, check updates or call
   DSH Inventory.
5. Stopped instances read cached snapshots; manual Refresh may deep-scan the
   profile as a repair path.

## Current coverage

| Resource | Current implementation | Gap for the next phase |
| --- | --- | --- |
| Plugins | `dsh plugin add/remove/update` plus `pluginInventory.list` snapshot exists | Make DSH Inventory the primary Library source and use profile scans only for manual repair/deep scan. |
| Skins / UI | Installed through `dsh plugin add`; Launcher records skin classification | Ensure every Market skin is first accepted by DSH, then classified as Skin/UI from metadata. |
| Skills | Launcher downloads files and records manifest entries | Add a DSH discovery check when running, so Library can distinguish installed files from DSH-visible skills. |
| MCP servers | Launcher writes Cordis patch rows and records manifest entries | Add a DSH discovery/config validation check when running, if DSH exposes one. |
| Bundles | Launcher imports bundle leaves one by one | Store bundle install plan/job metadata separately from runtime inventory. |

## Unified Market install entrypoint

Market must call one command for leaf resources:

```text
market_install(instanceId, registryEntry)
```

The command dispatches from the resource capability table:

1. Plugin and Skin/UI entries run through `dsh plugin add` first.
2. Skill entries write `SKILL.md` into the instance `skills/` folder first.
3. MCP entries write the DSH-recognized `mcp-client` patch row first.
4. Only after that succeeds, Launcher records Market metadata.
5. Finally Launcher reconciles `library-inventory.json`:
   - running instance: prefer live DSH `pluginInventory.list`;
   - stopped instance: rebuild from DSH workspace/profile files.

The older `plugin_install`, `skill_install` and `mcp_install` commands remain
for compatibility and non-Market management actions, but the Market surface
should not choose separate install paths itself.

## Library mixed inventory view

Library reads one instance-scoped mixed view:

```text
library_inventory_detail(instanceId)
```

Each row carries both a user-facing source and a technical state source:

| Source label | Meaning |
| --- | --- |
| `DSH native` | The row came from DSH Inventory and has no Launcher Market metadata. |
| `Market installed` | The row is matched with Launcher Market metadata recorded after a successful install. |
| `Local file` | The row is a DSH workspace file/config object such as a skill file or MCP patch row. |
| `Imported environment` | Reserved for environment-package import metadata. |
| `Unknown detected` | Launcher detected it from profile/cache, but cannot prove Market origin. |

| State source | Meaning |
| --- | --- |
| `DSH Inventory` | Runtime plugin state from DSH's `pluginInventory.list`. |
| `DSH workspace` | Files or config rows inside the instance workspace that DSH discovers. |
| `Launcher snapshot` | Cached fallback when DSH is not running or the item is classification metadata. |

Library may render cached rows immediately, but any running-instance plugin
truth must come from DSH Inventory after reconciliation.

## Heavy Task Queue

All slow profile/DSH operations must enter the backend heavy-task queue instead
of being launched independently from pages. The queue is scoped per instance, so
one instance cannot install, uninstall, sync inventory, diagnose, and check
updates at the same time.

Current queued task kinds:

- `launch`
- `install`
- `uninstall`
- `inventory-sync`
- `diagnostics`
- `update-check`
- `environment-import`
- `environment-export`
- `profile-mutation`

Startup keeps the product order explicit: launch DSH first, inject the usage
proxy as soon as the DSH URL is ready, then run DSH Inventory sync through the
same queue. Market metadata merge stays inside the install/import job that
created the change, then the refreshed snapshot is emitted to Library, Overview,
and Instances.

## Launch Hot Path

The launch path is intentionally minimal:

```text
launch instance
-> DSH web ready
-> show Workspace
```

Startup must not run diagnostics, update checks, hidden-page rendering, deep
profile scans, or node_modules scans. After the DSH URL is visible, background
maintenance runs in this order:

1. Immediately inject the usage proxy into the running DSH settings.
2. After about 2 seconds, run lightweight DSH Inventory sync through the heavy
   task queue.
3. After about 5 seconds, run non-first-paint maintenance such as model catalog,
   theme, and language sync.

Diagnostics and update checks are manual actions. They should never be started
implicitly by launch or Workspace/Manage mode switches.

## Install Center

Market starts install requests, but it does not own install state. Install
state lives in the global launcher store and is rendered by Install Center, so
page switches do not hide, reset, or cancel the task.

Install Center tracks these visible phases:

1. `downloading` - resolving/downloading the Market entry or manifest.
2. `dshInstalling` - applying the package through DSH or writing a DSH-recognized
   workspace file/config.
3. `inventorySync` - refreshing the snapshot after the backend task finishes.
4. `classifying` - applying Launcher metadata such as Market source and
   Library category.
5. `done` / `failed` - terminal state retained until the user clears it.

Each job keeps recent log lines, retry metadata, and actions for opening the
Library, revealing the instance workspace, or revealing the profile config.
