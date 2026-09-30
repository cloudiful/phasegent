// Window and renderer hosting options for the secure main-process composition.
//
// Kept free of runtime Electron access so the security-relevant values
// (context isolation, Node integration, sandbox, CSP, load target) are unit
// testable without starting a shell.

import type { BrowserWindowConstructorOptions } from 'electron'
import { rendererIndexUrl } from './renderer-protocol'

export const WINDOW_TITLE = 'phasegent'
export const WINDOW_WIDTH = 1024
export const WINDOW_HEIGHT = 768
export const WINDOW_MIN_WIDTH = 800
export const WINDOW_MIN_HEIGHT = 600

export interface WindowOptionsInput {
  /** Absolute path to the built preload bundle. */
  preloadPath: string
}

/**
 * Secure `BrowserWindow` options. The renderer is isolated and sandboxed, has
 * no Node integration, cannot create webviews, and never receives the raw IPC
 * surface; all backend access goes through the typed preload API.
 */
export function windowOptions(input: WindowOptionsInput): BrowserWindowConstructorOptions {
  return {
    title: WINDOW_TITLE,
    width: WINDOW_WIDTH,
    height: WINDOW_HEIGHT,
    minWidth: WINDOW_MIN_WIDTH,
    minHeight: WINDOW_MIN_HEIGHT,
    show: false,
    autoHideMenuBar: true,
    backgroundColor: '#0b0b0f',
    webPreferences: {
      preload: input.preloadPath,
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webviewTag: false,
      allowRunningInsecureContent: false,
    },
  }
}

/** Where the window loads its document: the dev server, else the packaged dist. */
export function rendererTarget(input: { devServerUrl?: string | null }): { url: string } {
  const dev = input.devServerUrl?.trim()
  return { url: dev && dev.length > 0 ? dev : rendererIndexUrl() }
}

/**
 * Content-Security-Policy for the renderer. The development variant additionally
 * permits the Vite dev server's inline bootstrap and HMR websocket; the
 * packaged policy allows nothing but the app's own origin.
 */
export function rendererCsp(input: { dev: boolean }): string {
  const directives = [
    "default-src 'self'",
    input.dev ? "script-src 'self' 'unsafe-inline'" : "script-src 'self'",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self' data:",
    input.dev ? "connect-src 'self' ws: wss:" : "connect-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
  ]
  return directives.join('; ')
}
