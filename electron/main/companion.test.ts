import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import {
  BACKEND_BINARY_NAME,
  BACKEND_OVERRIDE_ENV,
  CARGO_BINARY_NAME,
  CompanionNotFoundError,
  resolveCompanionBinary,
} from './companion'

const repoRoot = '/repo'

function dev(overrides: Partial<Parameters<typeof resolveCompanionBinary>[0]> = {}) {
  return resolveCompanionBinary({
    isPackaged: false,
    resourcesPath: '',
    appPath: repoRoot,
    platform: 'linux',
    exists: () => false,
    ...overrides,
  })
}

describe('companion binary resolution', () => {
  test('packaged runs read the extra resource next to the app', () => {
    expect(resolveCompanionBinary({
      isPackaged: true,
      resourcesPath: '/app/resources',
      appPath: '/app/resources/app.asar',
      platform: 'darwin',
    })).toBe(`/app/resources/${BACKEND_BINARY_NAME}`)
  })

  test('packaged Windows runs use the executable suffix', () => {
    expect(resolveCompanionBinary({
      isPackaged: true,
      resourcesPath: 'C:\\app\\resources',
      appPath: 'C:\\app\\resources\\app.asar',
      platform: 'win32',
    })).toContain('.exe')
  })

  test('the explicit override wins over every other source', () => {
    expect(dev({ override: '/custom/phasegent' })).toBe('/custom/phasegent')
    expect(dev({ override: '  /custom/phasegent  ' })).toBe('/custom/phasegent')
    expect(resolveCompanionBinary({
      isPackaged: true,
      resourcesPath: '/app/resources',
      appPath: '/app',
      override: '/custom/phasegent',
      platform: 'darwin',
    })).toBe('/custom/phasegent')
  })

  test('development prefers the debug build and falls back to release', () => {
    const debug = `/repo/target/debug/${CARGO_BINARY_NAME}`
    const release = `/repo/target/release/${CARGO_BINARY_NAME}`
    expect(dev({ exists: path => path === debug || path === release })).toBe(debug)
    expect(dev({ exists: path => path === release })).toBe(release)
  })

  test('development appends the executable suffix on Windows', () => {
    expect(dev({ platform: 'win32', exists: () => true })).toBe(
      `/repo/target/debug/${CARGO_BINARY_NAME}.exe`,
    )
  })

  test('a missing development build reports build guidance', () => {
    expect(() => dev()).toThrow(CompanionNotFoundError)
    expect(() => dev()).toThrow(new RegExp(BACKEND_OVERRIDE_ENV))
  })

  test('the packaged resource name never collides with the app executable', () => {
    expect(BACKEND_BINARY_NAME).not.toBe(CARGO_BINARY_NAME)
  })
})

describe('backend binary contract', () => {
  test('the Cargo binary name matches the repository manifest', () => {
    const manifest = readFileSync(new URL('../../Cargo.toml', import.meta.url), 'utf8')
    expect(manifest).toContain(`[[bin]]\nname = "${CARGO_BINARY_NAME}"`)
  })
})
