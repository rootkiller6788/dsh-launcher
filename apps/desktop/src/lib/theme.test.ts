import { beforeEach, describe, expect, it } from 'vitest'
import { applyTheme } from './theme'

describe('applyTheme', () => {
  beforeEach(() => {
    document.documentElement.dataset.theme = ''
  })

  it('resolves an explicit light theme to data-theme="light"', () => {
    applyTheme('light')
    expect(document.documentElement.dataset.theme).toBe('light')
  })

  it('resolves an explicit dark theme to data-theme="dark"', () => {
    applyTheme('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
  })

  it('resolves the system preference (light in tests) from the OS scheme', () => {
    applyTheme('system')
    expect(document.documentElement.dataset.theme).toBe('light')
  })
})
