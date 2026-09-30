// Vite build configuration for the Electron main process and preload script.
//
// Both entries are bundled separately: a sandboxed preload script can only
// require `electron`, so it must not depend on a shared chunk. `scripts/
// electron-build.mjs` runs both builds into `electron/dist`.

import { builtinModules } from 'node:module'
import type { InlineConfig } from 'vite'

/** Electron and Node built-ins stay external to the CJS bundles. */
const external = [
  'electron',
  ...builtinModules,
  ...builtinModules.map(name => `node:${name}`),
]

function bundle(entry: string, fileName: string): InlineConfig {
  return {
    // The config is consumed programmatically, so Vite resolves it against the
    // repository root instead of a config-relative root.
    configFile: false,
    build: {
      target: 'node20',
      outDir: 'electron/dist',
      emptyOutDir: false,
      minify: false,
      sourcemap: true,
      lib: {
        entry,
        formats: ['cjs'],
        fileName: () => `${fileName}.cjs`,
      },
      rollupOptions: {
        external,
      },
    },
  }
}

export const mainConfig = bundle('electron/main.ts', 'main')
export const preloadConfig = bundle('electron/preload.ts', 'preload')

export default mainConfig
