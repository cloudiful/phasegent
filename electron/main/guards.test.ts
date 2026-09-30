import { describe, expect, test } from 'bun:test'
import {
  installContentsGuards,
  installSessionGuards,
  isAllowedNavigationUrl,
  rendererAllowance,
  type ContentsLike,
  type NavigationAllowance,
  type NavigationEvent,
  type OnHeadersReceivedDetails,
  type SessionLike,
} from './guards'
import { rendererIndexUrl } from './renderer-protocol'

const packaged: NavigationAllowance = { devServerOrigin: null, rendererIndexUrl: rendererIndexUrl() }
const developing = rendererAllowance({ devServerUrl: 'http://localhost:1420/' })

describe('navigation allowance', () => {
  test('development allows only the dev server origin', () => {
    expect(developing.devServerOrigin).toBe('http://localhost:1420')
    expect(isAllowedNavigationUrl('http://localhost:1420/tasks', developing)).toBe(true)
    expect(isAllowedNavigationUrl('http://localhost:9999/tasks', developing)).toBe(false)
    expect(isAllowedNavigationUrl('app://renderer/index.html', developing)).toBe(false)
  })

  test('an unparsable dev URL leaves only the packaged allowance', () => {
    const allowance = rendererAllowance({ devServerUrl: 'not a url' })
    expect(allowance.devServerOrigin).toBeNull()
    expect(isAllowedNavigationUrl('not a url', allowance)).toBe(false)
  })

  test('packaged runs allow only the app scheme inside its own root', () => {
    expect(isAllowedNavigationUrl(rendererIndexUrl(), packaged)).toBe(true)
    expect(isAllowedNavigationUrl('app://renderer/index.html#/tasks', packaged)).toBe(true)
    expect(isAllowedNavigationUrl('app://renderer/assets/app.js', packaged)).toBe(true)
    expect(isAllowedNavigationUrl('app://other/index.html', packaged)).toBe(false)
    expect(isAllowedNavigationUrl('https://example.com/', packaged)).toBe(false)
    expect(isAllowedNavigationUrl('file:///etc/passwd', packaged)).toBe(false)
    expect(isAllowedNavigationUrl('javascript:alert(1)', packaged)).toBe(false)
    expect(isAllowedNavigationUrl('', packaged)).toBe(false)
  })
})

function fakeContents() {
  const navigate: Array<(event: NavigationEvent, url: string) => void> = []
  const attach: Array<(event: NavigationEvent) => void> = []
  let openHandler: ((details: { url: string }) => { action: 'deny' }) | null = null
  const contents: ContentsLike = {
    on: ((event: string, listener: unknown) => {
      if (event === 'will-navigate') navigate.push(listener as (event: NavigationEvent, url: string) => void)
      else attach.push(listener as (event: NavigationEvent) => void)
    }) as ContentsLike['on'],
    setWindowOpenHandler: handler => {
      openHandler = handler
    },
  }
  return {
    contents,
    navigateTo: (url: string) => {
      let prevented = false
      const event: NavigationEvent = { preventDefault: () => { prevented = true } }
      for (const listener of navigate) listener(event, url)
      return prevented
    },
    attachWebview: () => {
      let prevented = false
      for (const listener of attach) listener({ preventDefault: () => { prevented = true } })
      return prevented
    },
    openWindow: (url: string) => openHandler?.({ url }),
  }
}

describe('contents guards', () => {
  test('blocks navigation outside the app and allows the app document', () => {
    const fake = fakeContents()
    installContentsGuards(fake.contents, packaged)
    expect(fake.navigateTo(rendererIndexUrl())).toBe(false)
    expect(fake.navigateTo('https://example.com/phish')).toBe(true)
  })

  test('denies every window open request and every webview attachment', () => {
    const fake = fakeContents()
    installContentsGuards(fake.contents, packaged)
    expect(fake.openWindow('https://example.com')).toEqual({ action: 'deny' })
    expect(fake.attachWebview()).toBe(true)
  })
})

function fakeSession() {
  let filter: { urls: string[] } | null = null
  let listener: ((details: OnHeadersReceivedDetails, callback: (response: { responseHeaders: Record<string, string[]> }) => void) => void) | null = null
  const permissionRequests: boolean[] = []
  const session: SessionLike = {
    webRequest: {
      onHeadersReceived: (receivedFilter, receivedListener) => {
        filter = receivedFilter
        listener = receivedListener
      },
    },
    setPermissionRequestHandler: handler => {
      handler({}, 'media', granted => permissionRequests.push(granted))
    },
    setPermissionCheckHandler: handler => {
      permissionRequests.push(handler({}, 'geolocation', 'app://renderer'))
    },
  }
  return {
    session,
    filter: () => filter,
    applyHeaders: (details: OnHeadersReceivedDetails) => {
      let headers: Record<string, string[]> = {}
      listener?.(details, response => {
        headers = response.responseHeaders
      })
      return headers
    },
    permissionRequests,
  }
}

describe('session guards', () => {
  test('applies the CSP to every URL and replaces an existing policy', () => {
    const fake = fakeSession()
    installSessionGuards(fake.session, "default-src 'self'")
    expect(fake.filter()).toEqual({ urls: ['<all_urls>'] })
    const added = fake.applyHeaders({ url: 'app://renderer/index.html' })
    expect(added['Content-Security-Policy']).toEqual(["default-src 'self'"])
    const replaced = fake.applyHeaders({
      url: 'app://renderer/index.html',
      responseHeaders: { 'Content-Security-Policy': ["default-src 'none'"], ETag: ['x'] },
    })
    expect(replaced['Content-Security-Policy']).toEqual(["default-src 'self'"])
    expect(replaced.ETag).toEqual(['x'])
  })

  test('denies every permission request and check', () => {
    const fake = fakeSession()
    installSessionGuards(fake.session, "default-src 'self'")
    expect(fake.permissionRequests).toEqual([false, false])
  })
})
