// jsdom does not implement `window.matchMedia`, which `theme.ts` relies on to
// resolve the `system` preference against the OS scheme. Stub a fixed `light`
// media query so the `system` resolution is deterministic regardless of the
// runner's real color scheme.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList
}
