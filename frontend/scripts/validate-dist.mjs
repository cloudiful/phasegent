// Deterministic post-build check for the Tauri static dist.
// Fails the build when the emitted bundle is missing or unusable from a
// file-based host (absolute asset URLs break under frontendDist).

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

const dist = new URL('../dist/', import.meta.url)
const distPath = dist.pathname

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

console.log('validate-dist: PASS')
