import { describe, expect, it } from 'vitest'

import { isFatalBackendExit } from './backend-exit'

describe('isFatalBackendExit', () => {
  it('treats a local-mode crash as fatal', () => {
    expect(isFatalBackendExit({ code: 1, signal: null }, 'local')).toBe(true)
  })

  it('treats a crash with no known mode as fatal', () => {
    expect(isFatalBackendExit({ code: 1, signal: null }, null)).toBe(true)
    expect(isFatalBackendExit({ code: 1, signal: null }, undefined)).toBe(true)
  })

  it('ignores exits while attached to a remote gateway (#37869)', () => {
    expect(isFatalBackendExit({ code: 0, signal: null }, 'remote')).toBe(false)
    expect(isFatalBackendExit({ code: null, signal: 'SIGTERM' }, 'remote')).toBe(false)
  })

  it('ignores exits tagged remote in the payload even before the connection mode is known', () => {
    expect(isFatalBackendExit({ code: 1, signal: null, mode: 'remote' }, null)).toBe(false)
  })

  it('ignores deliberate teardowns (e.g. switching gateways) regardless of mode', () => {
    expect(isFatalBackendExit({ code: null, signal: 'SIGTERM', deliberate: true }, 'local')).toBe(false)
    expect(isFatalBackendExit({ code: null, signal: 'SIGTERM', deliberate: true, mode: 'local' }, null)).toBe(false)
  })

  it('handles a null/undefined payload defensively', () => {
    expect(isFatalBackendExit(null, 'local')).toBe(true)
    expect(isFatalBackendExit(undefined, 'remote')).toBe(false)
  })
})
