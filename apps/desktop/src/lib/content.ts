import type { ContentKind } from './types'

/** i18n key for each catalog content kind, shared by Instances and Market. */
export const KIND_LABEL: Record<ContentKind, string> = {
  plugin: 'market.tabPlugins',
  theme: 'market.tabThemes',
  skill: 'market.tabSkills',
  mcp: 'market.tabMcp',
  bundle: 'market.tabBundles',
}
