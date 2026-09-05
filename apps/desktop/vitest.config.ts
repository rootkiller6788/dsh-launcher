import { defineConfig } from 'vitest/config'

// Frontend unit tests (P4 #404): theme sync + market state machine live in the
// Zustand store and the pure `theme.ts` resolver, none of which need React's
// JSX transform or Tailwind, so this config stays minimal.
export default defineConfig({
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.ts'],
  },
})
