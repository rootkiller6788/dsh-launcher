use crate::market::ContentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAuthority {
    DshNative,
    LauncherManagedDshWorkspace,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAuthority {
    DshInventory,
    DshWorkspaceFiles,
    LauncherSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheSource {
    DshInventorySnapshot,
    InstanceManifest,
    MarketMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentCapability {
    pub kind: ContentKind,
    pub label: &'static str,
    pub install_authority: InstallAuthority,
    pub state_authority: StateAuthority,
    pub cache_sources: &'static [CacheSource],
    pub install_entrypoint: &'static str,
    pub scan_role: &'static str,
}

pub const CONTENT_CAPABILITIES: &[ContentCapability] = &[
    ContentCapability {
        kind: ContentKind::Plugin,
        label: "plugins",
        install_authority: InstallAuthority::DshNative,
        state_authority: StateAuthority::DshInventory,
        cache_sources: &[CacheSource::DshInventorySnapshot, CacheSource::MarketMetadata],
        install_entrypoint: "dsh plugin add/remove/toggle + pluginInventory.list",
        scan_role: "Launcher may classify, enrich and cache plugin rows, but DSH inventory decides whether a plugin exists and whether it is enabled.",
    },
    ContentCapability {
        kind: ContentKind::Theme,
        label: "skins/ui",
        install_authority: InstallAuthority::DshNative,
        state_authority: StateAuthority::DshInventory,
        cache_sources: &[
            CacheSource::DshInventorySnapshot,
            CacheSource::InstanceManifest,
            CacheSource::MarketMetadata,
        ],
        install_entrypoint: "dsh plugin add/remove/toggle; Launcher records skin classification metadata",
        scan_role: "Launcher classifies theme-like DSH plugins as skins/ui and records market origin, while DSH remains the activation source.",
    },
    ContentCapability {
        kind: ContentKind::Skill,
        label: "skills",
        install_authority: InstallAuthority::LauncherManagedDshWorkspace,
        state_authority: StateAuthority::DshWorkspaceFiles,
        cache_sources: &[CacheSource::InstanceManifest, CacheSource::MarketMetadata],
        install_entrypoint: "download SKILL.md into the instance skills/ folder recognized by DSH",
        scan_role: "Launcher downloads, validates and indexes skill files; DSH discovers and uses them from the workspace.",
    },
    ContentCapability {
        kind: ContentKind::Mcp,
        label: "mcp",
        install_authority: InstallAuthority::LauncherManagedDshWorkspace,
        state_authority: StateAuthority::DshWorkspaceFiles,
        cache_sources: &[CacheSource::InstanceManifest, CacheSource::MarketMetadata],
        install_entrypoint: "write the DSH-recognized MCP client config/patch row for the instance",
        scan_role: "Launcher validates catalog config and writes DSH-compatible MCP rows; DSH owns runtime discovery once mounted.",
    },
    ContentCapability {
        kind: ContentKind::Bundle,
        label: "bundles",
        install_authority: InstallAuthority::Composite,
        state_authority: StateAuthority::LauncherSnapshot,
        cache_sources: &[CacheSource::MarketMetadata],
        install_entrypoint: "expand into plugin/theme/skill/mcp leaf installers",
        scan_role: "Bundles are import plans, not runtime objects; Launcher tracks the plan and each installed leaf resource separately.",
    },
];

pub fn capability_for(kind: ContentKind) -> &'static ContentCapability {
    CONTENT_CAPABILITIES
        .iter()
        .find(|capability| capability.kind == kind)
        .expect("every ContentKind must have a capability boundary")
}
