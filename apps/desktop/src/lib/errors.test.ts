import { describe, expect, it } from 'vitest'
import { describeError } from './errors'

describe('describeError', () => {
  it('keeps the code and next action from a coded rejection', () => {
    const d = describeError({
      code: 'E2001',
      category: 'service',
      title: 'Plugin change failed',
      message: 'dsh plugin add exited with code 1',
      nextAction: 'Check the package name and your network, then retry.',
    })
    expect(d.code).toBe('E2001')
    expect(d.message).toContain('dsh plugin add')
    expect(d.nextAction).toContain('network')
  })

  it('falls back to a plain message for a string rejection', () => {
    const d = describeError('no network')
    expect(d).toEqual({ code: null, message: 'no network', nextAction: null })
  })

  it('reads the message from a plain Error without a code', () => {
    const d = describeError(new Error('boom'))
    expect(d.code).toBeNull()
    expect(d.message).toBe('boom')
    expect(d.nextAction).toBeNull()
  })
})
