// Build the Electron main-process and preload bundles with Vite.
//
// Usage: bun scripts/electron-build.mjs [--dist]
//
// Without flags it writes `electron/dist/main.cjs` and
// `electron/dist/preload.cjs`. `--dist` additionally builds the Vue renderer
// into `frontend/dist`, which is what packaging embeds.

import { readdirSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { build } from 'vite'
import { mainConfig, preloadConfig } from '../electron.vite.config.ts'

const outputDir = fileURLToPath(new URL('../electron/dist/', import.meta.url))
const frontendConfig = fileURLToPath(new URL('../frontend/vite.config.ts', import.meta.url))

/**
 * Bundle main and preload; each entry is one self-contained CJS file. A
 * sandboxed preload script can only require `electron`, so any extra emitted
 * chunk would be unloadable and fails the build instead.
 */
export async function buildElectronBundles() {
  rmSync(outputDir, { recursive: true, force: true })
  for (const config of [mainConfig, preloadConfig]) {
    await build(config)
  }
  const expected = new Set(['main.cjs', 'main.cjs.map', 'preload.cjs', 'preload.cjs.map'])
  const emitted = readdirSync(outputDir).filter(name => !expected.has(name))
  if (emitted.length > 0) {
    throw new Error(`electron-build: unexpected bundle outputs: ${emitted.join(', ')}`)
  }
  console.log(`electron-build: bundles written to ${outputDir}`)
}

/** Build the Vue renderer exactly as the existing frontend build does. */
export async function buildFrontendDist() {
  await build({ configFile: frontendConfig })
  console.log('electron-build: frontend dist rebuilt')
}

if (import.meta.main) {
  await buildElectronBundles()
  if (process.argv.includes('--dist')) await buildFrontendDist()
}
