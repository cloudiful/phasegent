// Correlated, bounded request/response client for the Rust stdio bridge.
//
// The client is transport-agnostic: it writes one JSON request line per call,
// matches responses by the `id` it assigned, and never assumes ordering. The
// child-process wiring lives in `child-transport.ts` and the process lifecycle
// in `../main/backend.ts`.

import {
  DESKTOP_PROTOCOL_VERSION,
  MAX_REQUEST_BYTES,
  type DesktopMethod,
} from '../shared/methods'

/** Failure kinds surfaced to the main process (and, bounded, to the renderer). */
export type BridgeFailureKind =
  | 'argument'
  | 'busy'
  | 'closed'
  | 'protocol'
  | 'timeout'
  | 'transport'

/** Bounded bridge failure; `message` never contains a request payload. */
export class BridgeFailure extends Error {
  readonly kind: BridgeFailureKind

  constructor(kind: BridgeFailureKind, message: string) {
    super(message)
    this.name = 'BridgeFailure'
    this.kind = kind
  }
}

/** A request line is written and responses are delivered here. */
export interface BridgeChannel {
  /** Write one line; `false` means the channel is gone. */
  write(line: string): boolean
  onMessage(listener: (line: string) => void): void
  onClose(listener: (reason: string) => void): void
  /** Ask the backend to shut down; the transport escalates if it does not. */
  close(): void
}

export interface BridgeClientOptions {
  /** Per-request deadline in milliseconds. */
  timeoutMs?: number
  /** Concurrent in-flight requests before new ones are rejected as busy. */
  maxInFlight?: number
  /** Bounded transport/protocol diagnostics (never request payloads). */
  onDiagnostic?: (message: string) => void
}

/** Provider reads can outlive a local call, so the deadline is generous but fixed. */
export const DEFAULT_TIMEOUT_MS = 120_000
export const MAX_IN_FLIGHT = 32

interface PendingRequest {
  resolve: (value: unknown) => void
  reject: (error: BridgeFailure) => void
  timer: ReturnType<typeof setTimeout> | undefined
}

interface BridgeResponse {
  id: number
  protocol: number
  ok: boolean
  result?: unknown
  error?: { kind?: unknown, message?: unknown }
}

export class BridgeClient {
  private readonly channel: BridgeChannel
  private readonly timeoutMs: number
  private readonly maxInFlight: number
  private readonly onDiagnostic: (message: string) => void
  private readonly pending = new Map<number, PendingRequest>()
  private nextId = 1
  private disposed = false

  constructor(channel: BridgeChannel, options: BridgeClientOptions = {}) {
    this.channel = channel
    this.timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS
    this.maxInFlight = options.maxInFlight ?? MAX_IN_FLIGHT
    this.onDiagnostic = options.onDiagnostic ?? (() => {})
    channel.onMessage(line => this.receive(line))
    channel.onClose(reason => {
      // The transport is gone: no later request can be served by this client, so
      // mark it closed and let the owner respawn the companion.
      this.disposed = true
      this.failAll('closed', bounded(reason))
    })
  }

  get inFlight(): number {
    return this.pending.size
  }

  get closed(): boolean {
    return this.disposed
  }

  /** Send one allowlisted request and resolve with the backend result. */
  request<T>(method: DesktopMethod, params: Record<string, unknown> = {}): Promise<T> {
    if (this.disposed) {
      return Promise.reject(new BridgeFailure('closed', 'desktop bridge is not running'))
    }
    if (this.pending.size >= this.maxInFlight) {
      return Promise.reject(
        new BridgeFailure('busy', `desktop bridge already has ${this.maxInFlight} requests in flight`),
      )
    }
    const line = JSON.stringify({ id: this.nextId, method, params })
    if (line.length > MAX_REQUEST_BYTES) {
      return Promise.reject(
        new BridgeFailure('argument', `request for ${method} exceeds ${MAX_REQUEST_BYTES} bytes`),
      )
    }
    const id = this.nextId
    this.nextId += 1
    const promise = new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new BridgeFailure('timeout', `${method} did not answer within ${this.timeoutMs} ms`))
      }, this.timeoutMs)
      // A pending request must never hold the event loop open by itself.
      timer.unref?.()
      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      })
    })
    if (!this.channel.write(line)) {
      // No usable transport remains: mark the client closed so its owner can
      // spawn a fresh companion instead of retrying a dead channel.
      this.disposed = true
      this.settle(id)?.reject(new BridgeFailure('closed', 'desktop bridge stopped accepting requests'))
    }
    return promise
  }

  /** Reject every pending request and close the transport. */
  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.failAll('closed', 'desktop bridge was shut down')
    this.channel.close()
  }

  private receive(line: string): void {
    if (line.trim().length === 0) return
    let parsed: unknown
    try {
      parsed = JSON.parse(line)
    }
    catch {
      this.fatal(`desktop bridge sent a non-JSON line (${line.length} bytes)`)
      return
    }
    const response = asResponse(parsed)
    if (!response) {
      this.fatal('desktop bridge sent a malformed response envelope')
      return
    }
    if (response.protocol !== DESKTOP_PROTOCOL_VERSION) {
      this.fatal(
        `desktop bridge protocol ${response.protocol} does not match ${DESKTOP_PROTOCOL_VERSION}`,
      )
      return
    }
    const entry = this.settle(response.id)
    if (!entry) {
      this.onDiagnostic(`desktop bridge answered unknown request id ${response.id}`)
      return
    }
    if (response.ok) {
      entry.resolve(response.result)
      return
    }
    const kind = typeof response.error?.kind === 'string' ? response.error.kind : 'transport'
    const message = typeof response.error?.message === 'string'
      ? response.error.message
      : 'desktop bridge reported an error without a message'
    entry.reject(new BridgeFailure('transport', bounded(`${kind}: ${message}`)))
  }

  private fatal(message: string): void {
    this.onDiagnostic(bounded(message))
    this.disposed = true
    this.failAll('protocol', bounded(message))
    this.channel.close()
  }

  private settle(id: number): PendingRequest | undefined {
    const entry = this.pending.get(id)
    if (!entry) return undefined
    this.pending.delete(id)
    if (entry.timer !== undefined) clearTimeout(entry.timer)
    return entry
  }

  private failAll(kind: BridgeFailureKind, message: string): void {
    const entries = [...this.pending.values()]
    this.pending.clear()
    for (const entry of entries) {
      if (entry.timer !== undefined) clearTimeout(entry.timer)
      entry.reject(new BridgeFailure(kind, message))
    }
  }
}

function asResponse(value: unknown): BridgeResponse | null {
  if (typeof value !== 'object' || value === null) return null
  const record = value as Record<string, unknown>
  if (typeof record.id !== 'number' || typeof record.protocol !== 'number') return null
  if (typeof record.ok !== 'boolean') return null
  if (record.ok) return record as unknown as BridgeResponse
  const error = record.error
  if (typeof error !== 'object' || error === null) return null
  return record as unknown as BridgeResponse
}

/** Bound any diagnostic so a chatty backend cannot flood the log. */
function bounded(message: string, max = 512): string {
  return message.length > max ? `${message.slice(0, max)}…` : message
}
