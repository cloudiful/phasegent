import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const validatorUrl = new URL('../scripts/validate-dist.mjs', import.meta.url)
const source = readFileSync(validatorUrl, 'utf8')

describe('validate-dist Windows path regression', () => {
  test('resolves frontend/dist with a cross-platform file URL conversion', () => {
    expect(source).toContain("from 'node:url'")
    expect(source).toContain('fileURLToPath')
    expect(source).toContain('fileURLToPath(new URL(')
    // URL.pathname is not a Windows filesystem path (leading slash,
    // percent-encoding, forward slashes), so it must not back filesystem access.
    expect(source).not.toContain('.pathname')
  })

  test('conversion agrees with the local dist layout', () => {
    const distPath = fileURLToPath(new URL('../dist/', import.meta.url))
    expect(distPath).toContain('frontend')
    expect(distPath).toContain('dist')
  })

  test('still enforces the generated frontend/dist contract', () => {
    expect(source).toContain("join(distPath, 'index.html')")
    expect(source).toContain('frontend/dist/index.html is missing')
    expect(source).toContain('absolute asset URLs')
    expect(source).toContain('vite base must stay relative')
    expect(source).toContain("join(distPath, 'assets')")
    expect(source).toContain('frontend/dist/assets/ is missing.')
    expect(source).toContain("endsWith('.js')")
    expect(source).toContain("endsWith('.css')")
    expect(source).toContain('no JavaScript bundle found in frontend/dist/assets/.')
    expect(source).toContain('no CSS bundle found in frontend/dist/assets/.')
    expect(source).toContain('empty bundle emitted:')
    expect(source).toContain('index.html does not reference the emitted JavaScript bundle.')
    expect(source).toContain('validate-dist: PASS')
  })
})
