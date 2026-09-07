import { describe, expect, test } from 'bun:test'
import { spawnSync } from 'node:child_process'
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const validatorUrl = new URL('../scripts/validate-dist.mjs', import.meta.url)
const validatorPath = fileURLToPath(validatorUrl)
const source = readFileSync(validatorUrl, 'utf8')

function writeFixture(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), 'phasegent-dist-'))
  for (const [relative, content] of Object.entries(files)) {
    const target = join(root, relative)
    mkdirSync(join(target, '..'), { recursive: true })
    writeFileSync(target, content)
  }
  return root
}

function runValidator(distDir: string): { status: number; output: string } {
  const result = spawnSync(process.execPath, [validatorPath, distDir], {
    encoding: 'utf8',
  })
  return {
    status: result.status ?? 1,
    output: `${result.stdout ?? ''}${result.stderr ?? ''}`,
  }
}

function healthyFixture(): Record<string, string> {
  return {
    'index.html':
      '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
      '<link rel="stylesheet" href="./assets/app-abc.css"></head><body><div id="app"></div></body></html>',
    'assets/app-abc.js': 'console.log("app");',
    // Minimal stylesheet carrying the shell-critical markers the validator requires.
    'assets/app-abc.css': ':root{--ui-bg:#fff}.flex{display:flex}.bg-default{background:var(--ui-bg)}',
  }
}

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

  test('enforces the unstyled-GUI contract (CSS reference, assets, markers)', () => {
    expect(source).toContain('index.html does not reference the emitted CSS bundle')
    expect(source).toContain('references missing asset')
    expect(source).toContain('requiredCssMarkers')
    expect(source).toContain('--ui-bg')
    expect(source).toContain('.flex')
    expect(source).toContain('bg-default')
    expect(source).toContain('may be purged')
    // Split-CSS repair: markers are checked on aggregated entry-linked
    // stylesheets, and non-stylesheet CSS references are rejected.
    expect(source).toContain('stylesheetRefs')
    expect(source).toContain('aggregatedCss')
    expect(source).toContain('without rel=stylesheet')
    expect(source).toContain('is not a CSS file')
  })
})

describe('validate-dist regression fixtures', () => {
  test('healthy fixture passes', () => {
    const dir = writeFixture(healthyFixture())
    const result = runValidator(dir)
    expect(result.status).toBe(0)
    expect(result.output).toContain('validate-dist: PASS')
  })

  test('missing CSS reference fails as unstyled', () => {
    const files = healthyFixture()
    files['index.html'] =
      '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script></head>' +
      '<body><div id="app"></div></body></html>'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('does not reference the emitted CSS bundle')
  })

  test('missing referenced asset fails', () => {
    const files = healthyFixture()
    files['index.html'] = files['index.html'].replace(
      '</head>',
      '<script type="module" src="./assets/missing-xyz.js"></script></head>',
    )
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('references missing asset')
  })

  test('purged stylesheet without markers fails', () => {
    const files = healthyFixture()
    files['assets/app-abc.css'] = ':root{--x:1}.other{display:block}'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('missing expected')
  })

  test('absolute asset URLs fail', () => {
    const files = healthyFixture()
    files['index.html'] =
      '<!doctype html><html><head><script type="module" src="/assets/app-abc.js"></script>' +
      '<link rel="stylesheet" href="/assets/app-abc.css"></head><body></body></html>'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('absolute asset URLs')
  })

  test('split entry stylesheets with distributed markers pass', () => {
    const dir = writeFixture({
      'index.html':
        '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
        '<link rel="stylesheet" href="./assets/entry-one.css">' +
        '<link rel="stylesheet" href="./assets/entry-two.css"></head><body></body></html>',
      'assets/app-abc.js': 'console.log("app");',
      // Neither chunk alone carries every marker; together they do.
      'assets/entry-one.css': ':root{--ui-bg:#fff}.flex{display:flex}',
      'assets/entry-two.css': '.bg-default{background:var(--ui-bg)}',
    })
    const result = runValidator(dir)
    expect(result.status).toBe(0)
    expect(result.output).toContain('validate-dist: PASS')
  })

  test('unreferenced CSS chunk without markers is ignored', () => {
    const files = healthyFixture()
    files['assets/lazy-chunk.css'] = ':root{--x:1}.other{display:block}'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).toBe(0)
    expect(result.output).toContain('validate-dist: PASS')
  })

  test('split entry stylesheets still missing a marker fail', () => {
    const dir = writeFixture({
      'index.html':
        '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
        '<link rel="stylesheet" href="./assets/entry-one.css">' +
        '<link rel="stylesheet" href="./assets/entry-two.css"></head><body></body></html>',
      'assets/app-abc.js': 'console.log("app");',
      'assets/entry-one.css': ':root{--ui-bg:#fff}.flex{display:flex}',
      'assets/entry-two.css': '.other{display:block}',
    })
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('missing expected')
  })

  test('missing entry stylesheet fails as missing asset', () => {
    const files = healthyFixture()
    files['index.html'] =
      '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
      '<link rel="stylesheet" href="./assets/app-abc.css">' +
      '<link rel="stylesheet" href="./assets/missing-chunk.css"></head><body></body></html>'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('references missing asset')
  })

  test('stylesheet reference to a non-CSS file fails', () => {
    const files = healthyFixture()
    files['index.html'] =
      '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
      '<link rel="stylesheet" href="./assets/app-abc.css">' +
      '<link rel="stylesheet" href="./assets/app-abc.txt"></head><body></body></html>'
    files['assets/app-abc.txt'] = ':root{--ui-bg:#fff}.flex{display:flex}.bg-default{}'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('is not a CSS file')
  })

  test('CSS href without rel=stylesheet fails as unstyled', () => {
    const files = healthyFixture()
    files['index.html'] =
      '<!doctype html><html><head><script type="module" src="./assets/app-abc.js"></script>' +
      '<link href="./assets/app-abc.css"></head><body></body></html>'
    const dir = writeFixture(files)
    const result = runValidator(dir)
    expect(result.status).not.toBe(0)
    expect(result.output).toContain('does not reference the emitted CSS bundle')
  })
})
