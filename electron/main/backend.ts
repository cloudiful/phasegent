// Backend lifecycle for the main process: one lazily spawned Rust companion,
// respawned after an exit, and disposed with the app.
//
// The bridge is spawned on the first request so a missing companion surfaces as
// a bounded per-request failure instead of an unusable window.

import {
  BridgeClient,
  BridgeFailure,
  type BridgeClientOptions,
} from '../bridge/bridge-client'
import { spawnBridgeChannel, type SpawnFn } from '../bridge/child-transport'
import type { DesktopMethod } from '../shared/methods'

export interface BackendBridgeOptions {
  /** Absolute path to the companion binary; resolved per request. */
  resolveBinary: () => string
  spawn?: SpawnFn
  clientOptions?: BridgeClientOptions
  /** Bounded transport diagnostics; never carries request payloads. */
  onDiagnostic?: (message: string) => void
}

export class BackendBridge {
  private readonly options: BackendBridgeOptions
  private client: BridgeClient | null = null
  private spawns = 0

  constructor(options: BackendBridgeOptions) {
    this.options = options
  }

  /** Number of spawned companions; exposed for diagnostics and tests. */
  get spawnCount(): number {
    return this.spawns
  }

  get running(): boolean {
    return this.client !== null && !this.client.closed
  }

  async request<T>(method: DesktopMethod, params: Record<string, unknown> = {}): Promise<T> {
    const client = this.client ?? this.start()
    try {
      return await client.request<T>(method, params)
    }
    catch (error) {
      // A dead companion must not poison later requests: drop the client so the
      // next call spawns a fresh one.
      if (this.client === client && client.closed) this.client = null
      throw error
    }
  }

  /** Stop the companion and reject every in-flight request. */
  dispose(): void {
    const client = this.client
    this.client = null
    client?.dispose()
  }

  private start(): BridgeClient {
    if (this.client && !this.client.closed) return this.client
    let binary: string
    try {
      binary = this.options.resolveBinary()
    }
    catch (error) {
      throw new BridgeFailure('closed', error instanceof Error ? error.message : String(error))
    }
    const client = new BridgeClient(
      spawnBridgeChannel({
        binary,
        spawn: this.options.spawn,
        onDiagnostic: this.options.onDiagnostic,
      }),
      this.options.clientOptions,
    )
    this.client = client
    this.spawns += 1
    return client
  }
}
