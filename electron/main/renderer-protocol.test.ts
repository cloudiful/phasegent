import { describe, expect, test } from 'bun:test'
import {
  RENDERER_INDEX_PATH,
  installRendererProtocol,
  rendererIndexUrl,
  rendererRootDir,
  resolveRendererRequestPath,
  type ProtocolLike,
} from './renderer-protocol'

const root = '/app/frontend/dist'

describe('renderer request paths', () => {
  test('maps the root document and nested assets inside the dist root', () => {
    expect(rendererIndexUrl()).toBe('app://renderer/index.html')
    expect(rendererRootDir('/app')).toBe(root)
    expect(resolveRendererRequestPath('app://renderer/index.html', root)).toBe(`${root}/index.html`)
    expect(resolveRendererRequestPath('app://renderer/', root)).toBe(`${root}/${RENDERER_INDEX_PATH}`)
    expect(resolveRendererRequestPath('app://renderer/assets/app.js', root)).toBe(`${root}/assets/app.js`)
    expect(resolveRendererRequestPath('app://renderer/index.html?x=1#/tasks', root)).toBe(
      `${root}/index.html`,
    )
  })

  test('rejects another scheme, another host, and unparsable URLs', () => {
    expect(resolveRendererRequestPath('file:///etc/passwd', root)).toBeNull()
    expect(resolveRendererRequestPath('app://evil/index.html', root)).toBeNull()
    expect(resolveRendererRequestPath('https://renderer/index.html', root)).toBeNull()
    expect(resolveRendererRequestPath('::::', root)).toBeNull()
  })

  test('rejects encoded traversal and backslash escapes', () => {
    expect(resolveRendererRequestPath('app://renderer/%2e%2e%2fsecret.txt', root)).toBeNull()
    expect(resolveRendererRequestPath('app://renderer/a/..%2F..%2Fsecret', root)).toBeNull()
    expect(resolveRendererRequestPath('app://renderer/..%5Csecret', root)).toBeNull()
    expect(resolveRendererRequestPath('app://renderer/a%5Cb.css', root)).toBeNull()
  })

  test('normalises duplicate, dot, and parent segments inside the root', () => {
    expect(resolveRendererRequestPath('app://renderer/./assets//app.js', root)).toBe(
      `${root}/assets/app.js`,
    )
    // The URL parser already collapses leading `..`, so the request can never
    // leave the root even before the segment guard runs.
    expect(resolveRendererRequestPath('app://renderer/../../etc/passwd', root)).toBe(
      `${root}/etc/passwd`,
    )
  })
})

function fakeProtocol() {
  const handlers = new Map<string, (request: { url: string }) => Promise<unknown>>()
  const protocol: ProtocolLike = {
    handle: (scheme, handler) => {
      handlers.set(scheme, handler)
    },
  }
  return {
    protocol,
    handle: (url: string) => handlers.get('app')!({ url }),
  }
}

describe('renderer protocol handler', () => {
  test('serves files inside the dist root and attaches the CSP', async () => {
    const fake = fakeProtocol()
    const requested: string[] = []
    installRendererProtocol({
      protocol: fake.protocol,
      rootDir: root,
      csp: "default-src 'self'",
      fetchFile: async path => {
        requested.push(path)
        return new Response(`body:${path}`, { headers: { 'content-type': 'text/html' } })
      },
    })
    const response = await fake.handle('app://renderer/assets/app.js') as Response
    expect(await response.text()).toBe(`body:${root}/assets/app.js`)
    expect(requested).toEqual([`${root}/assets/app.js`])
    expect(response.headers.get('Content-Security-Policy')).toBe("default-src 'self'")
    expect(response.headers.get('content-type')).toBe('text/html')
  })

  test('answers 404 outside the root and never calls the reader', async () => {
    const fake = fakeProtocol()
    const diagnostics: string[] = []
    let reads = 0
    installRendererProtocol({
      protocol: fake.protocol,
      rootDir: root,
      fetchFile: async () => {
        reads += 1
        return new Response('')
      },
      onDiagnostic: message => diagnostics.push(message),
    })
    const response = await fake.handle('app://renderer/%2e%2e%2fsecret') as Response
    expect(response).toBeInstanceOf(Response)
    expect(response.status).toBe(404)
    expect(reads).toBe(0)
    expect(diagnostics).toHaveLength(1)
  })

  test('answers 404 when the reader fails or returns no response', async () => {
    const failing = fakeProtocol()
    installRendererProtocol({
      protocol: failing.protocol,
      rootDir: root,
      fetchFile: async () => {
        throw new Error('ENOENT')
      },
    })
    expect((await failing.handle('app://renderer/missing.js') as Response).status).toBe(404)

    const empty = fakeProtocol()
    const diagnostics: string[] = []
    installRendererProtocol({
      protocol: empty.protocol,
      rootDir: root,
      fetchFile: async () => ({}),
      onDiagnostic: message => diagnostics.push(message),
    })
    expect((await empty.handle('app://renderer/missing.js') as Response).status).toBe(404)
    expect(diagnostics).toHaveLength(1)
  })
})
