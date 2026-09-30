import { describe, expect, test } from 'bun:test'
import { rendererIndexUrl } from './renderer-protocol'
import { WINDOW_HEIGHT, WINDOW_WIDTH, rendererCsp, rendererTarget, windowOptions } from './window'

describe('window options', () => {
  const options = windowOptions({ preloadPath: '/app/electron/dist/preload.cjs' })

  test('isolates the renderer and disables Node integration', () => {
    expect(options.webPreferences?.contextIsolation).toBe(true)
    expect(options.webPreferences?.nodeIntegration).toBe(false)
    expect(options.webPreferences?.sandbox).toBe(true)
  })

  test('disables webviews and insecure content, and wires the preload', () => {
    expect(options.webPreferences?.webviewTag).toBe(false)
    expect(options.webPreferences?.allowRunningInsecureContent).toBe(false)
    expect(options.webPreferences?.preload).toBe('/app/electron/dist/preload.cjs')
  })

  test('starts hidden until the renderer is ready', () => {
    expect(options.show).toBe(false)
    expect(options.width).toBe(WINDOW_WIDTH)
    expect(options.height).toBe(WINDOW_HEIGHT)
  })
})

describe('renderer target', () => {
  test('uses the dev server URL when present', () => {
    expect(rendererTarget({ devServerUrl: 'http://localhost:1420/' })).toEqual({
      url: 'http://localhost:1420/',
    })
  })

  test('falls back to the packaged renderer for empty or missing dev URLs', () => {
    expect(rendererTarget({ devServerUrl: null }).url).toBe(rendererIndexUrl())
    expect(rendererTarget({}).url).toBe(rendererIndexUrl())
    expect(rendererTarget({ devServerUrl: '   ' }).url).toBe(rendererIndexUrl())
  })
})

describe('renderer CSP', () => {
  const dev = rendererCsp({ dev: true })
  const packaged = rendererCsp({ dev: false })

  test('packaged policy allows only the app origin', () => {
    expect(packaged).toContain("default-src 'self'")
    expect(packaged).toContain("script-src 'self';")
    expect(packaged).toContain("connect-src 'self';")
    expect(packaged).not.toContain('ws:')
    expect(packaged).not.toContain("script-src 'self' 'unsafe-inline'")
  })

  test('development policy additionally permits the Vite dev server', () => {
    expect(dev).toContain("script-src 'self' 'unsafe-inline'")
    expect(dev).toContain("connect-src 'self' ws: wss:")
  })

  test('both policies lock down objects, base URLs, forms, and framing', () => {
    for (const policy of [dev, packaged]) {
      expect(policy).toContain("object-src 'none'")
      expect(policy).toContain("base-uri 'none'")
      expect(policy).toContain("form-action 'none'")
      expect(policy).toContain("frame-ancestors 'none'")
    }
  })
})
