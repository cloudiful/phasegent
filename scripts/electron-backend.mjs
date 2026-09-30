// Stage the compiled Rust companion binary as the Electron Builder extra
// resource consumed by `electron-builder.yml`.
//
// Usage: bun scripts/electron-backend.mjs [--from <path>] [--out <path>]
//
// Defaults to `target/release/phasegent[.exe]` (falling back to the debug
// build) and writes `target/companion/phasegent[.exe]`. Release workflows pass
// the per-target Cargo output explicitly, for example
// `--from target/aarch64-apple-darwin/release/phasegent`.

import { chmodSync, copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = fileURLToPath(new URL('..', import.meta.url))
const suffix = process.platform === 'win32' ? '.exe' : ''

function flag(name) {
  const index = process.argv.indexOf(name)
  return index >= 0 ? process.argv[index + 1] : undefined
}

const requested = flag('--from')
const from = requested
  ? resolve(requested)
  : [join(repoRoot, 'target', 'release', `phasegent${suffix}`), join(repoRoot, 'target', 'debug', `phasegent${suffix}`)]
      .find(candidate => existsSync(candidate))

if (!from || !existsSync(from)) {
  console.error(
    `electron-backend: FAIL: companion binary not found; build it with 'cargo build --release --bin phasegent' or pass --from <path>`,
  )
  process.exit(1)
}

const out = resolve(flag('--out') ?? join(repoRoot, 'target', 'companion', `phasegent${suffix}`))
mkdirSync(dirname(out), { recursive: true })
copyFileSync(from, out)
if (process.platform !== 'win32') chmodSync(out, 0o755)

console.log(`electron-backend: ${from} -> ${out}`)
