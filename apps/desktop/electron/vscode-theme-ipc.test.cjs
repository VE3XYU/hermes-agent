'use strict'

const assert = require('node:assert/strict')
const test = require('node:test')

const { registerVscodeThemeIpc } = require('./vscode-theme-ipc.cjs')

function fakeIpcMain() {
  const handlers = new Map()

  return {
    handlers,
    handle(channel, handler) {
      assert.ok(!handlers.has(channel), `duplicate registration for ${channel}`)
      handlers.set(channel, handler)
    }
  }
}

test('registerVscodeThemeIpc wires only hermes:vscode-theme:* channels, each to a handler fn', () => {
  const ipcMain = fakeIpcMain()

  registerVscodeThemeIpc({ ipcMain })

  assert.deepEqual([...ipcMain.handlers.keys()].sort(), ['hermes:vscode-theme:fetch', 'hermes:vscode-theme:search'])

  for (const handler of ipcMain.handlers.values()) {
    assert.equal(typeof handler, 'function')
  }
})
