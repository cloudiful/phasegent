// Electron main process: window lifecycle, security guards, renderer hosting,
// and the single Rust companion bridge.
//
// The renderer is sandboxed and isolated; it reaches the backend only through
// the typed preload API. All Rust domain work happens in the companion process.

import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { BrowserWindow, app, ipcMain, net, protocol, session } from 'electron'
import { BackendBridge } from './main/backend'
import { BACKEND_OVERRIDE_ENV, resolveCompanionBinary } from './main/companion'
import {
  installContentsGuards,
  installSessionGuards,
  rendererAllowance,
} from './main/guards'
import { registerDesktopIpc } from './main/ipc'
import {
  RENDERER_SCHEME,
  installRendererProtocol,
  rendererRootDir,
} from './main/renderer-protocol'
import { rendererCsp, rendererTarget, windowOptions } from './main/window'

const devServerUrl = process.env.PHASEGENT_DEV_SERVER_URL?.trim() || null
// `resourcesPath` is added by the Electron runtime, not by Node's typings.
const resourcesPath = (process as unknown as { resourcesPath?: string }).resourcesPath ?? ''
const diagnostic = (scope: string) => (message: string) => {
  console.error(`[phasegent:${scope}] ${message}`)
}

const bridge = new BackendBridge({
  resolveBinary: () =>
    resolveCompanionBinary({
      isPackaged: app.isPackaged,
      resourcesPath,
      appPath: app.getAppPath(),
      override: process.env[BACKEND_OVERRIDE_ENV] ?? null,
    }),
  onDiagnostic: diagnostic('bridge'),
})

// The privileged scheme must be registered before the app is ready.
protocol.registerSchemesAsPrivileged([
  {
    scheme: RENDERER_SCHEME,
    privileges: { standard: true, secure: true, supportFetchAPI: true, codeCache: true },
  },
])

let mainWindow: BrowserWindow | null = null

function createWindow(): void {
  const window = new BrowserWindow({
    ...windowOptions({ preloadPath: join(__dirname, 'preload.cjs') }),
  })
  window.once('ready-to-show', () => window.show())
  window.on('closed', () => {
    mainWindow = null
  })
  const { url } = rendererTarget({ devServerUrl })
  void window.loadURL(url).catch(error => {
    console.error(`[phasegent:window] could not load the renderer: ${String(error)}`)
  })
  mainWindow = window
}

if (!app.requestSingleInstanceLock()) {
  app.quit()
}
else {
  app.on('second-instance', () => {
    if (!mainWindow) return
    if (mainWindow.isMinimized()) mainWindow.restore()
    mainWindow.focus()
  })

  // Every web contents — including anything created later — gets the
  // navigation, window-open, and webview guards.
  app.on('web-contents-created', (_event, contents) => {
    installContentsGuards(contents, rendererAllowance({ devServerUrl }))
  })

  app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') app.quit()
  })

  app.on('will-quit', () => bridge.dispose())

  void app.whenReady().then(() => {
    const csp = rendererCsp({ dev: devServerUrl !== null })
    installSessionGuards(session, csp)
    installRendererProtocol({
      protocol,
      rootDir: rendererRootDir(app.getAppPath()),
      csp,
      fetchFile: path => net.fetch(pathToFileURL(path).toString()),
      onDiagnostic: diagnostic('renderer'),
    })
    registerDesktopIpc({ ipcMain, bridge, onError: diagnostic('ipc') })
    createWindow()
    app.on('activate', () => {
      if (BrowserWindow.getAllWindows().length === 0) createWindow()
    })
  })
}
