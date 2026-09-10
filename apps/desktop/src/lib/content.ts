import type { ContentKind, RegistryPlugin } from './types'

/** Stable identity: `owner/name` when an owner exists, else the bare name. */
export function pluginKey(p: RegistryPlugin) {
  return p.owner ? `${p.owner}/${p.name}` : p.name
}

/** i18n key for each catalog content kind, shared by Instances and Market. */
export const KIND_LABEL: Record<ContentKind, string> = {
  plugin: 'market.tabPlugins',
  theme: 'market.tabThemes',
  skill: 'market.tabSkills',
  mcp: 'market.tabMcp',
  bundle: 'market.tabBundles',
}
