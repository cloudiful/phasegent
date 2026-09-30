// Development runner for the Electron shell.
//
// Usage: bun scripts/electron-dev.mjs [--backend <path>]
//
// Starts the Vite dev server, builds the main/preload bundles, and launches
// Electron with the dev server URL plus the resolved Rust companion path. The
// dev server is closed when Electron exits.

import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { buildElectronBundles } from './electron-build.mjs'
import { BACKEND_OVERRIDE_ENV, resolveCompanionBinary } from '../electron/main/companion.ts'

const repoRoot = fileURLToPath(new URL('..', import.meta.url))
const frontendConfig = fileURLToPath(new URL('../frontend/vite.config.ts', import.meta.url))

function backendFlag() {
  const index = process.argv.indexOf('--backend')
  return index >= 0 ? process.argv[index + 1] : null
}

let backend
try {
  backend = resolveCompanionBinary({
    isPackaged: false,
    resourcesPath: '',
    appPath: repoRoot,
    override: backendFlag(),
  })
}
catch (error) {
  console.error(`electron-dev: FAIL: ${error instanceof Error ? error.message : String(error)}`)
  process.exit(1)
}

let electronPath
try {
  const loaded = await import('electron')
  electronPath = typeof loaded.default === 'string' ? loaded.default : null
}
catch {
  electronPath = null
}
if (!electronPath) {
  console.error('electron-dev: FAIL: the electron devDependency is not installed; run bun install')
  process.exit(1)
}

await buildElectronBundles()

const server = await createServer({ configFile: frontendConfig })
await server.listen()
const devServerUrl = server.resolvedUrls?.local?.[0]
if (!devServerUrl) {
  console.error('electron-dev: FAIL: the Vite dev server did not report a local URL')
  await server.close()
  process.exit(1)
}
console.log(`electron-dev: renderer ${devServerUrl}`)
console.log(`electron-dev: backend  ${backend}`)

const child = spawn(electronPath, [repoRoot], {
  stdio: 'inherit',
  env: {
    ...process.env,
    PHASEGENT_DEV_SERVER_URL: devServerUrl,
    [BACKEND_OVERRIDE_ENV]: backend,
  },
})

let shuttingDown = false
const stop = async (code) => {
  if (shuttingDown) return
  shuttingDown = true
  await server.close()
  process.exit(code)
}

child.on('error', error => {
  console.error(`electron-dev: FAIL: could not start Electron: ${error.message}`)
  void stop(1)
})
child.on('exit', code => {
  void stop(code ?? 0)
})
process.on('SIGINT', () => {
  child.kill('SIGINT')
  void stop(130)
})
