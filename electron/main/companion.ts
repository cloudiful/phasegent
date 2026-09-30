// Locate the Rust companion binary for development and packaged runs.
//
// Packaged apps read the binary from `process.resourcesPath`, where Electron
// Builder copies the Cargo output as an extra resource under a non-conflicting
// backend filename. Development runs use the Cargo target directory unless
// `PHASEGENT_DESKTOP_BACKEND` overrides the path explicitly.

import { existsSync } from 'node:fs'
import { join } from 'node:path'

/** Packaged resource name; never the app executable's own name. */
export const BACKEND_BINARY_NAME = 'phasegent-backend'

/** Cargo binary name resolved from `target/{debug,release}` in development. */
export const CARGO_BINARY_NAME = 'phasegent'

/** Development override read from the environment. */
export const BACKEND_OVERRIDE_ENV = 'PHASEGENT_DESKTOP_BACKEND'

/** Raised when no companion binary exists; the message is renderer-safe. */
export class CompanionNotFoundError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'CompanionNotFoundError'
  }
}

export interface CompanionResolutionInput {
  /** True for a packaged app (`app.isPackaged`). */
  isPackaged: boolean
  /** `process.resourcesPath` for packaged runs. */
  resourcesPath: string
  /** `app.getAppPath()`, the repository root during development. */
  appPath: string
  /** Explicit override, usually [`BACKEND_OVERRIDE_ENV`]. */
  override?: string | null
  platform?: NodeJS.Platform
  /** Existence probe; injectable so resolution stays testable. */
  exists?: (path: string) => boolean
}

function candidatePaths(input: CompanionResolutionInput, suffix: string): string[] {
  return [
    join(input.appPath, 'target', 'debug', `${CARGO_BINARY_NAME}${suffix}`),
    join(input.appPath, 'target', 'release', `${CARGO_BINARY_NAME}${suffix}`),
  ]
}

/**
 * Resolve the companion binary path.
 *
 * @throws CompanionNotFoundError when a development build has not been
 * compiled yet, so the operator gets build guidance instead of a spawn error.
 */
export function resolveCompanionBinary(input: CompanionResolutionInput): string {
  const override = input.override?.trim()
  if (override) return override

  const platform = input.platform ?? process.platform
  const suffix = platform === 'win32' ? '.exe' : ''
  if (input.isPackaged) {
    return join(input.resourcesPath, `${BACKEND_BINARY_NAME}${suffix}`)
  }

  const exists = input.exists ?? existsSync
  const candidates = candidatePaths(input, suffix)
  const found = candidates.find(candidate => exists(candidate))
  if (!found) {
    throw new CompanionNotFoundError(
      `desktop backend not found; build it with 'cargo build --bin phasegent' or set ${BACKEND_OVERRIDE_ENV}`,
    )
  }
  return found
}
