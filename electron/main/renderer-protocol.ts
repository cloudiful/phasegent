// Renderer hosting: the packaged app serves `frontend/dist` over a privileged
// `app://renderer` scheme instead of `file://`.
//
// A standard scheme gives the packaged renderer a real origin, so the
// Content-Security-Policy in `guards.ts` and relative asset URLs keep their
// normal meaning. Only paths inside the dist root are served.

import { resolve, sep } from 'node:path'

export const RENDERER_SCHEME = 'app'
export const RENDERER_HOST = 'renderer'
export const RENDERER_INDEX_PATH = 'index.html'

/** Origin of the packaged renderer, used for navigation and CSP decisions. */
export function rendererIndexUrl(): string {
  return `${RENDERER_SCHEME}://${RENDERER_HOST}/${RENDERER_INDEX_PATH}`
}

/** Directory served by the renderer protocol. */
export function rendererRootDir(appPath: string): string {
  return resolve(appPath, 'frontend', 'dist')
}

/**
 * Map one `app://renderer/...` request to an absolute file path inside
 * `rootDir`, or `null` for any other scheme/host, an encoded traversal, or a
 * path that escapes the dist root. `/` maps to the index document.
 */
export function resolveRendererRequestPath(requestUrl: string, rootDir: string): string | null {
  let parsed: URL
  try {
    parsed = new URL(requestUrl)
  }
  catch {
    return null
  }
  if (parsed.protocol !== `${RENDERER_SCHEME}:` || parsed.hostname !== RENDERER_HOST) return null

  let decoded: string
  try {
    decoded = decodeURIComponent(parsed.pathname)
  }
  catch {
    return null
  }
  const segments = decoded
    .split('/')
    .filter(segment => segment.length > 0 && segment !== '.')
  if (segments.some(segment => segment === '..' || segment.includes('\\') || segment.includes('\0'))) {
    return null
  }

  const root = resolve(rootDir)
  const relative = segments.length > 0 ? segments.join(sep) : RENDERER_INDEX_PATH
  const absolute = resolve(root, relative)
  if (absolute !== root && !absolute.startsWith(`${root}${sep}`)) return null
  return absolute
}

export interface ProtocolLike {
  handle(scheme: string, handler: (request: { url: string }) => Promise<unknown>): void
}

export interface RendererProtocolOptions {
  protocol: ProtocolLike
  /** Reads one file (packaged: `net.fetch` over a `file://` URL). */
  fetchFile: (path: string) => Promise<unknown>
  rootDir: string
  /** Policy attached to every served document and asset. */
  csp?: string
  onDiagnostic?: (message: string) => void
}

/**
 * Serve the packaged renderer. Every rejection answers with a plain 404 and a
 * bounded diagnostic; the request URL is never echoed into the page. The CSP is
 * attached here as well as through the session guard so the packaged policy
 * holds regardless of how the custom scheme is intercepted.
 */
export function installRendererProtocol(options: RendererProtocolOptions): void {
  const root = resolve(options.rootDir)
  const onDiagnostic = options.onDiagnostic ?? (() => {})
  const notFound = () => new Response('Not found', { status: 404 })
  options.protocol.handle(RENDERER_SCHEME, async (request) => {
    const path = resolveRendererRequestPath(request.url, root)
    if (path === null) {
      onDiagnostic('renderer request rejected: outside the packaged dist root')
      return notFound()
    }
    let response: unknown
    try {
      response = await options.fetchFile(path)
    }
    catch {
      return notFound()
    }
    if (!(response instanceof Response)) {
      onDiagnostic('renderer read did not produce a response')
      return notFound()
    }
    if (!options.csp) return response
    const headers = new Headers(response.headers)
    headers.set('Content-Security-Policy', options.csp)
    return new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers,
    })
  })
}
