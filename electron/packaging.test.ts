// Packaging contract: Electron Builder must ship the same identity, renderer,
// bundles, and Rust companion the main process resolves at runtime, and the
// package scripts must wire the build inputs those files expect.

import { readFileSync } from 'node:fs'
import { describe, expect, test } from 'bun:test'
import { BACKEND_BINARY_NAME } from './main/companion'

const read = (relative: string) => readFileSync(new URL(relative, import.meta.url), 'utf8')
const builder = Bun.YAML.parse(read('../electron-builder.yml')) as Record<string, any>
const pkg = JSON.parse(read('../package.json')) as Record<string, any>

describe('electron-builder configuration', () => {
  test('keeps the existing desktop identity and output directory', () => {
    expect(builder.appId).toBe('com.cloud1ful.phasegent')
    expect(builder.productName).toBe('phasegent')
    expect(builder.directories.output).toBe('dist/electron')
    expect(builder.asar).toBe(true)
  })

  test('ships the built main/preload bundles and the renderer dist', () => {
    expect(builder.files).toContain('electron/dist/**')
    expect(builder.files).toContain('frontend/dist/**')
    expect(pkg.main).toBe('electron/dist/main.cjs')
  })

  test('bundles the Rust companion as an extra resource for each desktop target', () => {
    expect(builder.mac.extraResources).toContainEqual({
      from: 'target/companion/phasegent',
      to: BACKEND_BINARY_NAME,
    })
    expect(builder.win.extraResources).toContainEqual({
      from: 'target/companion/phasegent.exe',
      to: `${BACKEND_BINARY_NAME}.exe`,
    })
  })

  test('preserves the Windows MSI and macOS DMG release targets with icons', () => {
    expect(builder.mac.target).toContain('dmg')
    expect(builder.win.target).toContain('msi')
    expect(builder.mac.icon).toBe('icons/icon.icns')
    expect(builder.win.icon).toBe('icons/icon.ico')
    expect(builder.msi.createStartMenuShortcut).toBe(true)
    expect(builder.msi.createDesktopShortcut).toBe(true)
  })

  test('leaves Linux and the container image out of desktop packaging', () => {
    expect(builder.linux).toBeUndefined()
  })

  test('pins the Electron toolchain in package.json', () => {
    expect(pkg.devDependencies.electron).toMatch(/^\d+\.\d+\.\d+$/)
    expect(pkg.devDependencies['electron-builder']).toMatch(/^\d+\.\d+\.\d+$/)
  })

  test('wires the build, dev, backend, packaging, test, and typecheck scripts', () => {
    expect(pkg.scripts['electron:build']).toBe('bun scripts/electron-build.mjs')
    expect(pkg.scripts['electron:dev']).toBe('bun scripts/electron-dev.mjs')
    expect(pkg.scripts['electron:backend']).toBe('bun scripts/electron-backend.mjs')
    expect(pkg.scripts['electron:package']).toContain('electron-builder --config electron-builder.yml')
    expect(pkg.scripts['test:electron']).toBe('bun test electron')
    expect(pkg.scripts.test).toContain('electron')
    expect(pkg.scripts.typecheck).toContain('tsconfig.electron.json')
  })
})

describe('application identity parity with the Rust shell', () => {
  test('the builder appId matches the identifier reported by the backend', () => {
    const gui = read('../src/gui/mod.rs')
    const identifier = gui.match(/identifier: "([^"]+)"/)
    expect(identifier?.[1]).toBe(builder.appId)
  })
})
