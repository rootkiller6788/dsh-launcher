import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ipc } from '../lib/ipc'
import type { Registry, RegistryPlugin, RecommendResult } from '../lib/types'
import { useAppStore } from './appStore'

// The store reaches into the Tauri event bus and the Rust command layer; both
// are mocked so these tests exercise the Zustand state machine in isolation.
// Defaults resolve (rather than reject) so the store's fire-and-forget `.then`/
// `.catch` chains on `setTheme`/`setSettings` never hit `undefined`.
vi.mock('../lib/ipc', () => ({
  ipc: {
    setTheme: vi.fn().mockResolvedValue('dark'),
    dshTheme: vi.fn().mockResolvedValue(null),
    setSettings: vi.fn().mockResolvedValue({}),
    setLanguage: vi.fn().mockResolvedValue('en'),
    dshLanguage: vi.fn().mockResolvedValue(null),
    marketRegistry: vi.fn().mockResolvedValue(null),
    marketRecommend: vi.fn().mockResolvedValue(null),
    marketInstall: vi.fn().mockResolvedValue(null),
  },
}))

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}))

const FIXED_NOW = new Date('2026-09-04T12:00:00Z')

beforeEach(() => {
  vi.useFakeTimers()
  vi.setSystemTime(FIXED_NOW)
  vi.clearAllMocks()
  useAppStore.setState({
    theme: 'system',
    settings: null,
    error: null,
    activeId: null,
    registry: null,
    registryError: null,
    recommendations: null,
    searching: false,
  })
  document.documentElement.dataset.theme = ''
})

afterEach(() => {
  vi.useRealTimers()
})

describe('theme sync', () => {
  it('setTheme updates state, applies the resolved theme, and persists', () => {
    useAppStore.setState({ settings: { language: 'en' } })
    useAppStore.getState().setTheme('dark')

    expect(useAppStore.getState().theme).toBe('dark')
    expect(useAppStore.getState().settings?.theme).toBe('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(vi.mocked(ipc.setTheme)).toHaveBeenCalledWith('dark')
  })

  it('syncTheme is a no-op inside the write cooldown', async () => {
    useAppStore.getState().setTheme('dark')
    await useAppStore.getState().syncTheme()

    expect(vi.mocked(ipc.dshTheme)).not.toHaveBeenCalled()
  })

  it('syncTheme adopts the DSH theme after the cooldown and persists it', async () => {
    useAppStore.setState({ settings: { language: 'en' } })
    vi.advanceTimersByTime(3000)
    vi.mocked(ipc.dshTheme).mockResolvedValueOnce('light')
    await useAppStore.getState().syncTheme()

    expect(useAppStore.getState().theme).toBe('light')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(vi.mocked(ipc.setSettings)).toHaveBeenCalled()
  })

  it('syncTheme ignores a missing or invalid DSH value', async () => {
    vi.advanceTimersByTime(3000)
    vi.mocked(ipc.dshTheme).mockResolvedValueOnce('sepia')
    await useAppStore.getState().syncTheme()

    expect(useAppStore.getState().theme).toBe('system')
    expect(vi.mocked(ipc.setSettings)).not.toHaveBeenCalled()
  })
})

describe('market state machine', () => {
  it('loadRegistry populates the registry and clears the error', async () => {
    const registry = { updated: '2026-09-04', count: 1, categories: {} } as unknown as Registry
    vi.mocked(ipc.marketRegistry).mockResolvedValueOnce(registry)
    await useAppStore.getState().loadRegistry()

    expect(useAppStore.getState().registry).toEqual(registry)
    expect(useAppStore.getState().registryError).toBeNull()
  })

  it('loadRegistry surfaces a fetch failure as registryError', async () => {
    vi.mocked(ipc.marketRegistry).mockRejectedValueOnce(new Error('offline'))
    await useAppStore.getState().loadRegistry()

    expect(useAppStore.getState().registry).toBeNull()
    expect(useAppStore.getState().registryError).toContain('offline')
  })

  it('recommend toggles searching and stores the result', async () => {
    const result = { plans: [], candidates: [], raw: 'ok' } as unknown as RecommendResult
    vi.mocked(ipc.marketRecommend).mockResolvedValueOnce(result)

    const pending = useAppStore.getState().recommend('sql tuning')
    expect(useAppStore.getState().searching).toBe(true)

    await pending
    expect(useAppStore.getState().searching).toBe(false)
    expect(useAppStore.getState().recommendations).toEqual(result)
    expect(vi.mocked(ipc.marketRecommend)).toHaveBeenCalledWith('sql tuning')
  })

  it('installMarketEntry refuses when no instance is active', async () => {
    const ok = await useAppStore.getState().installMarketEntry({} as unknown as RegistryPlugin)

    expect(ok).toBe(false)
    expect(vi.mocked(ipc.marketInstall)).not.toHaveBeenCalled()
  })

  it('installMarketEntry enqueues the install for the active instance', async () => {
    useAppStore.setState({ activeId: 'inst-1' })
    const entry = {
      name: 'mcp-x',
      owner: 'acme',
      url: 'https://example.com/x',
    } as unknown as RegistryPlugin

    const ok = await useAppStore.getState().installMarketEntry(entry)

    expect(ok).toBe(true)
    expect(vi.mocked(ipc.marketInstall)).toHaveBeenCalledWith('inst-1', entry)
  })

  it('installMarketEntry surfaces a backend rejection as an error', async () => {
    useAppStore.setState({ activeId: 'inst-1' })
    vi.mocked(ipc.marketInstall).mockRejectedValueOnce(new Error('no network'))

    const ok = await useAppStore.getState().installMarketEntry({} as unknown as RegistryPlugin)

    expect(ok).toBe(false)
    expect(useAppStore.getState().error).toContain('no network')
  })
})
