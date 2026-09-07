// Deterministic post-build check for the Tauri static dist.
// Fails the build when the emitted bundle is missing or unusable from a
// file-based host (absolute asset URLs break under frontendDist).

import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const defaultDistPath = fileURLToPath(new URL('../dist/', import.meta.url))
// Optional first argument overrides the dist directory for regression fixtures;
// release and `bun run validate` pass no argument and keep the default.
const distPath = process.argv[2] ? resolve(process.argv[2]) : defaultDistPath

function fail(message) {
  console.error(`validate-dist: FAIL: ${message}`)
  process.exit(1)
}

let entries = []
let index = ''
try {
  index = readFileSync(join(distPath, 'index.html'), 'utf8')
}
catch {
  fail('frontend/dist/index.html is missing; run the Vite build first.')
}

if (/(src="\/|href="\/)/.test(index)) {
  fail('index.html contains absolute asset URLs; vite base must stay relative (./).')
}

const assetsPath = join(distPath, 'assets')
try {
  entries = readdirSync(assetsPath)
}
catch {
  fail('frontend/dist/assets/ is missing.')
}

const js = entries.filter(name => name.endsWith('.js'))
const css = entries.filter(name => name.endsWith('.css'))
if (js.length === 0) fail('no JavaScript bundle found in frontend/dist/assets/.')
if (css.length === 0) fail('no CSS bundle found in frontend/dist/assets/.')

for (const name of [...js, ...css]) {
  const size = statSync(join(assetsPath, name)).size
  if (size === 0) fail(`empty bundle emitted: assets/${name}`)
  console.log(`validate-dist: assets/${name} (${size} bytes)`)
}

if (!js.some(name => index.includes(name))) {
  fail('index.html does not reference the emitted JavaScript bundle.')
}

if (!css.some(name => index.includes(name))) {
  fail('index.html does not reference the emitted CSS bundle; the installed GUI would render unstyled.')
}

// Every relative asset referenced by index.html must exist in dist;
// a missing stylesheet or script is the direct unstyled/broken-GUI failure.
const referenced = [...index.matchAll(/(?:src|href)="([^"]+)"/g)]
  .map(match => match[1])
  .filter(ref => ref && !/^(?:https?:|data:|blob:|#)/.test(ref))
for (const ref of referenced) {
  const clean = ref.split(/[?#]/)[0]
  if (!clean || clean === './' || clean === '/') continue
  // Absolute URLs already failed above; only file-based dist assets are checked.
  if (clean.startsWith('/')) continue
  if (!existsSync(join(distPath, clean))) {
    fail(`index.html references missing asset: ${ref}`)
  }
}

// Entry-linked stylesheets only: Vite may split CSS into multiple chunks
// (code-splitting, lazy routes). Markers can be distributed across chunks,
// so aggregate the actual rel=stylesheet content linked by index.html instead
// of requiring every emitted CSS file to contain every marker.
const linkTags = [...index.matchAll(/<link\b[^>]*>/gi)].map(match => match[0])
const stylesheetRefs = []
for (const tag of linkTags) {
  const rel = tag.match(/\brel\s*=\s*["']([^"']*)["']/i)
  if (!rel || !rel[1].toLowerCase().split(/\s+/).includes('stylesheet')) continue
  const href = tag.match(/\bhref\s*=\s*["']([^"']+)["']/i)
  if (!href) {
    fail('index.html contains a stylesheet link without href; the installed GUI would render unstyled.')
  }
  stylesheetRefs.push(href[1])
}

if (stylesheetRefs.length === 0) {
  fail('index.html does not reference the emitted CSS bundle; the installed GUI would render unstyled.')
}

let aggregatedCss = ''
for (const ref of stylesheetRefs) {
  if (/^(?:https?:|data:|blob:|#)/.test(ref)) {
    fail(`index.html stylesheet reference is not a local dist asset: ${ref}`)
  }
  const clean = ref.split(/[?#]/)[0]
  if (!clean || clean === './' || clean === '/') {
    fail(`index.html stylesheet reference is not a CSS file: ${ref}`)
  }
  if (clean.startsWith('/')) {
    fail(`index.html stylesheet reference is not a relative dist asset: ${ref}`)
  }
  if (!clean.toLowerCase().endsWith('.css')) {
    fail(`index.html stylesheet reference is not a CSS file: ${ref}`)
  }
  const target = join(distPath, clean)
  if (!existsSync(target)) {
    fail(`index.html references missing asset: ${ref}`)
  }
  aggregatedCss += `${readFileSync(target, 'utf8')}\n`
}

// A CSS file referenced without rel=stylesheet never applies: reject it
// explicitly instead of passing on substring presence.
const linkedStylesheets = new Set(stylesheetRefs.map(ref => ref.split(/[?#]/)[0]))
for (const match of index.matchAll(/\bhref\s*=\s*["']([^"']+)["']/gi)) {
  const ref = match[1]
  const clean = ref.split(/[?#]/)[0]
  if (!clean.toLowerCase().endsWith('.css')) continue
  if (/^(?:https?:|data:|blob:|#)/.test(ref)) continue
  if (!linkedStylesheets.has(clean)) {
    fail(`index.html references CSS without rel=stylesheet: ${ref}; the installed GUI would render unstyled.`)
  }
}

// The aggregated entry-linked stylesheet must contain shell-critical
// Tailwind/Nuxt UI markers; tokens alone without utilities means the GUI
// renders unstyled.
const requiredCssMarkers = ['--ui-bg', '.flex', 'bg-default']
for (const marker of requiredCssMarkers) {
  if (!aggregatedCss.includes(marker)) {
    fail(`CSS bundles linked by index.html are missing expected ${marker}; Tailwind/Nuxt UI styles may be purged.`)
  }
}

console.log('validate-dist: PASS')
