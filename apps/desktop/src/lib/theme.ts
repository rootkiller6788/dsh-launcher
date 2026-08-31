import type { Theme } from './types'

let current: Theme = 'system'
let media: MediaQueryList | null = null

/**
 * Apply `theme` to the document by setting `data-theme` on `<html>` — the CSS
 * palette swap (dark default / `html[data-theme="light"]` override) keys off
 * that attribute. While the preference is `system`, a `prefers-color-scheme`
 * listener re-resolves when the OS scheme flips (same as DSH's own runtime).
 */
export function applyTheme(theme: Theme): void {
  current = theme
  if (typeof document === 'undefined') return
  if (!media) {
    media = window.matchMedia('(prefers-color-scheme: dark)')
    media.addEventListener('change', () => {
      if (current === 'system') applyTheme('system')
    })
  }
  const resolved: 'light' | 'dark' = theme === 'system' ? (media.matches ? 'dark' : 'light') : theme
  document.documentElement.dataset.theme = resolved
}
