// Security guards for the renderer: navigation, window creation, webview
// attachment, CSP headers, and permission requests.
//
// The guards take narrow structural views of the Electron objects so the
// decisions stay unit testable: navigation is allowed only to the app's own
// document root (or the dev server origin during development), no window is
// opened from the renderer, no webview is attached, and every permission
// request is denied.

import { rendererIndexUrl } from './renderer-protocol'

export interface NavigationAllowance {
  /** Origin of the Vite dev server, or `null` for a packaged run. */
  devServerOrigin: string | null
  /** Index document URL that defines the allowed packaged origin and root. */
  rendererIndexUrl: string
}

/** Allowance derived from the runtime environment (dev server or packaged). */
export function rendererAllowance(input: { devServerUrl?: string | null }): NavigationAllowance {
  const dev = input.devServerUrl?.trim()
  let devServerOrigin: string | null = null
  if (dev && dev.length > 0) {
    try {
      devServerOrigin = new URL(dev).origin
    }
    catch {
      devServerOrigin = null
    }
  }
  return { devServerOrigin, rendererIndexUrl: rendererIndexUrl() }
}

/** True when `url` is the app's own document (its scheme, host, and root). */
export function isAllowedNavigationUrl(url: string, allowance: NavigationAllowance): boolean {
  let parsed: URL
  let renderer: URL
  try {
    parsed = new URL(url)
    renderer = new URL(allowance.rendererIndexUrl)
  }
  catch {
    return false
  }
  // Comparisons stay on protocol/host/path: the packaged `app:` scheme is not a
  // "special" scheme, so its `origin` is opaque outside the renderer.
  if (allowance.devServerOrigin !== null) return parsed.origin === allowance.devServerOrigin
  if (parsed.protocol !== renderer.protocol || parsed.hostname !== renderer.hostname) return false
  const root = renderer.pathname.slice(0, renderer.pathname.lastIndexOf('/') + 1)
  return parsed.pathname.startsWith(root)
}

export interface NavigationEvent {
  preventDefault(): void
}

export interface ContentsLike {
  on(event: 'will-navigate', listener: (event: NavigationEvent, url: string) => void): void
  on(event: 'will-attach-webview', listener: (event: NavigationEvent) => void): void
  setWindowOpenHandler(handler: (details: { url: string }) => { action: 'deny' }): void
}

/** Install the navigation and window-creation guards for one web contents. */
export function installContentsGuards(contents: ContentsLike, allowance: NavigationAllowance): void {
  contents.on('will-navigate', (event, url) => {
    if (!isAllowedNavigationUrl(url, allowance)) event.preventDefault()
  })
  contents.on('will-attach-webview', event => {
    event.preventDefault()
  })
  contents.setWindowOpenHandler(() => ({ action: 'deny' }))
}

export interface OnHeadersReceivedDetails {
  url: string
  responseHeaders?: Record<string, string[] | undefined>
}

export interface SessionLike {
  webRequest: {
    onHeadersReceived(
      filter: { urls: string[] },
      listener: (
        details: OnHeadersReceivedDetails,
        callback: (response: { responseHeaders: Record<string, string[]> }) => void,
      ) => void,
    ): void
  }
  setPermissionRequestHandler(
    handler: (contents: unknown, permission: string, callback: (granted: boolean) => void) => void,
  ): void
  setPermissionCheckHandler(
    handler: (contents: unknown, permission: string, requestingOrigin: string) => boolean,
  ): void
}

/**
 * Apply the renderer CSP to every response and deny every permission request:
 * the desktop shell needs no camera, microphone, geolocation, or notification
 * access, and the Rust companion performs all network work.
 */
export function installSessionGuards(session: SessionLike, csp: string): void {
  session.webRequest.onHeadersReceived({ urls: ['<all_urls>'] }, (details, callback) => {
    callback({
      responseHeaders: { ...details.responseHeaders, 'Content-Security-Policy': [csp] },
    })
  })
  session.setPermissionRequestHandler((_contents, _permission, callback) => callback(false))
  session.setPermissionCheckHandler(() => false)
}
