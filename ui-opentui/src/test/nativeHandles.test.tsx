/**
 * Degrade path for native handle-table exhaustion (boundary/nativeHandles.ts).
 *
 * @opentui/core 0.4.0 routes every native object through ONE global
 * 65,534-slot handle registry; each TextBufferRenderable allocates a
 * SyntaxStyle (+ TextBuffer + TextBufferView) in its constructor, so a long
 * mount-everything transcript exhausts the table and `SyntaxStyle.create()`
 * throws `Failed to create SyntaxStyle` out of a Solid mount effect —
 * uncaught, then MASKED by the renderer's own error handler crashing while
 * allocating its console-overlay buffer (Node exit 7). The shim makes style
 * allocation DEGRADE: a detached style (native handle 0) whose JS-side style
 * definitions still work and whose native calls are inert no-ops, so text
 * still mounts and renders — merely unstyled at the buffer level.
 *
 * The failing factory injected here simulates the exhausted table for EVERY
 * style created in this fork (vitest isolates files per process, so the
 * global patch cannot leak into other suites).
 */
import { SyntaxStyle, TextBuffer } from '@opentui/core'
import { describe, expect, test } from 'vitest'

import { installSyntaxStyleDegrade } from '../boundary/nativeHandles.ts'
import { createSessionStore } from '../logic/store.ts'
import { App } from '../view/App.tsx'
import { ThemeProvider } from '../view/theme.tsx'
import { renderProbe } from './lib/render.ts'

// Simulate the exhausted handle table: the exact error zig.ts:4554 throws.
installSyntaxStyleDegrade(() => {
  throw new Error('Failed to create SyntaxStyle')
})

describe('native handle exhaustion degrades instead of crashing (boundary/nativeHandles.ts)', () => {
  test('SyntaxStyle.create() under allocation failure returns a detached, inert style — never throws', () => {
    let style!: SyntaxStyle
    expect(() => {
      style = SyntaxStyle.create()
    }).not.toThrow()

    // detached = native INVALID_HANDLE (0): every native call no-ops safely
    expect(Number(style.ptr)).toBe(0)
    expect(() => style.resolveStyleId('keyword')).not.toThrow()
    expect(style.getStyleCount()).toBe(0)

    // JS-side style definitions still work — markdown/code chunk colors are
    // computed from styleDefs/mergeStyles in JS, not the native handle.
    expect(() => style.registerStyle('keyword', { fg: '#ff0000', bold: true })).not.toThrow()
    expect(style.getStyle('keyword')).toMatchObject({ bold: true })
    expect(style.mergeStyles('keyword').fg).toBeDefined()

    expect(() => style.destroy()).not.toThrow()
  })

  test('a text buffer accepts a detached style (native treats handle 0 as "no style")', () => {
    const style = SyntaxStyle.create()
    const buffer = TextBuffer.create('unicode')
    try {
      expect(() => buffer.setSyntaxStyle(style)).not.toThrow()
      expect(() => buffer.setSyntaxStyle(null)).not.toThrow()
    } finally {
      buffer.destroy()
      style.destroy()
    }
  })

  test('the transcript still mounts and renders text when every style allocation fails', async () => {
    const store = createSessionStore()
    store.apply({ type: 'gateway.ready' })
    store.pushUser('does styled text survive handle exhaustion?')
    store.apply({ type: 'message.start' })
    store.apply({ type: 'tool.start', payload: { tool_id: 't1', name: 'terminal', context: 'echo degraded' } })
    store.apply({
      type: 'tool.complete',
      payload: {
        tool_id: 't1',
        name: 'terminal',
        args: { command: 'echo degraded' },
        duration_s: 0.1,
        result: 'degraded but alive'
      }
    })
    store.apply({ type: 'message.complete' })

    const errors: unknown[] = []
    const onErr = (e: unknown) => errors.push(e)
    process.on('uncaughtException', onErr)
    const probe = await renderProbe(
      () => (
        <ThemeProvider theme={() => store.state.theme}>
          <App store={store} />
        </ThemeProvider>
      ),
      { width: 100, height: 30 }
    )
    try {
      // Every TextBufferRenderable in this tree got a detached SyntaxStyle —
      // the content must still be there (unstyled is fine; absent/crashed is not).
      // (Assistant MARKDOWN paint is not assertable headlessly — tree-sitter
      // doesn't settle in the test renderer, see render.test.tsx — so assert
      // the plain-text user row and the styled tool row.)
      const frame = await probe.waitForFrame(f => f.includes('terminal'))
      expect(frame).toContain('does styled text survive handle exhaustion?')
      expect(errors).toEqual([])
    } finally {
      process.off('uncaughtException', onErr)
      probe.destroy()
    }
  }, 30000)
})
