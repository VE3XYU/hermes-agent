'use strict'

const { fetchMarketplaceThemes, searchMarketplaceThemes } = require('./vscode-marketplace.cjs')

// VS Code Marketplace color-theme IPC: fetch a theme by extension id + search the
// marketplace. Both delegate to the vscode-marketplace sibling module; no theme
// code is ever executed (only JSON is read from the .vsix).
function registerVscodeThemeIpc({ ipcMain }) {
  // Download a VS Code Marketplace extension and return the raw color-theme JSON
  // it contributes. No theme code is executed — we only read JSON from the .vsix.
  ipcMain.handle('hermes:vscode-theme:fetch', async (_event, id) => fetchMarketplaceThemes(String(id || '')))

  // Search the Marketplace for color-theme extensions (empty query = top installs).
  ipcMain.handle('hermes:vscode-theme:search', async (_event, query) =>
    searchMarketplaceThemes(String(query || ''), 20)
  )
}

module.exports = { registerVscodeThemeIpc }
