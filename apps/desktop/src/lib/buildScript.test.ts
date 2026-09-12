import { describe, expect, it } from 'vitest'
import { suggestBuildPackage } from './buildScript'

describe('suggestBuildPackage', () => {
  it('takes the first package pnpm named', () => {
    expect(
      suggestBuildPackage('Ignored build scripts: esbuild, sharp.\nRun "pnpm approve-builds"'),
    ).toBe('esbuild')
  })

  it('keeps a scoped name whole', () => {
    expect(suggestBuildPackage('WARN  Ignored build scripts: @scope/pkg.')).toBe('@scope/pkg')
  })

  it('finds the line wherever it sits in the tail', () => {
    expect(
      suggestBuildPackage('ERR_PNPM_...\n\n   Ignored build scripts: sharp\n\n  more output'),
    ).toBe('sharp')
  })

  it('offers nothing when pnpm said nothing about build scripts', () => {
    expect(suggestBuildPackage('ERR_PNPM_NO_MATCHING_VERSION  No matching version found')).toBe('')
    expect(suggestBuildPackage(null)).toBe('')
    expect(suggestBuildPackage('')).toBe('')
  })

  it('offers nothing when what followed the colon is not a package name', () => {
    // The line is pnpm's to change; anything that is not a plausible name is
    // left for the user to type rather than put in the box as if it were it.
    expect(suggestBuildPackage('Ignored build scripts: (2 packages)')).toBe('')
    expect(suggestBuildPackage('ignored build script: some sentence here')).toBe('')
  })
})
