// Main-process IPC surface: one handler per allowlisted desktop method.
//
// The renderer never supplies a channel, a method name, or a CLI argument — the
// preload script maps typed calls onto these channels, and the handler shapes
// them into the bridge `params` object. Payload validation here is structural
// and bounded; field-level validation and redaction stay in the Rust backend so
// there is exactly one source of truth.

import { BridgeFailure } from '../bridge/bridge-client'
import {
  DESKTOP_METHODS,
  DESKTOP_METHOD_NAMES,
  MAX_PAYLOAD_BYTES,
  isDesktopMethod,
  type DesktopMethod,
} from '../shared/methods'

/** Bridge view needed by the IPC layer. */
export interface DesktopInvoker {
  request<T>(method: DesktopMethod, params?: Record<string, unknown>): Promise<T>
}

export interface IpcMainLike {
  handle(channel: string, listener: (event: unknown, payload?: unknown) => unknown): void
}

export interface RegisterDesktopIpcOptions {
  ipcMain: IpcMainLike
  bridge: DesktopInvoker
  /** Bounded diagnostic hook; never receives payload values. */
  onError?: (message: string) => void
}

/** Register the ten allowlisted channels; nothing else becomes reachable. */
export function registerDesktopIpc(options: RegisterDesktopIpcOptions): void {
  for (const method of DESKTOP_METHOD_NAMES) {
    options.ipcMain.handle(DESKTOP_METHODS[method].channel, async (_event, payload) => {
      try {
        return await options.bridge.request(method, buildParams(method, payload))
      }
      catch (error) {
        const message = describeCallFailure(method, error)
        options.onError?.(message)
        throw new Error(message)
      }
    })
  }
}

/**
 * Shape a renderer payload into the bridge `params` object.
 *
 * @throws BridgeFailure with kind `argument` for non-object payloads, payloads
 * over the bound, and parameters sent to a method that takes none.
 */
export function buildParams(method: DesktopMethod, payload: unknown): Record<string, unknown> {
  if (!isDesktopMethod(method)) {
    throw new BridgeFailure('argument', `unsupported desktop method '${String(method)}'`)
  }
  const spec = DESKTOP_METHODS[method]
  if (payload === undefined || payload === null) {
    return spec.param === null ? {} : { [spec.param]: {} }
  }
  if (typeof payload !== 'object' || Array.isArray(payload)) {
    throw new BridgeFailure('argument', `${method} expects an object payload`)
  }
  const size = serializedSize(payload)
  if (size === null || size > MAX_PAYLOAD_BYTES) {
    throw new BridgeFailure('argument', `${method} payload exceeds ${MAX_PAYLOAD_BYTES} bytes`)
  }
  if (spec.param === null) {
    if (Object.keys(payload).length > 0) {
      throw new BridgeFailure('argument', `${method} takes no parameters`)
    }
    return {}
  }
  return { [spec.param]: payload }
}

/** Renderer-facing failure text: bounded, method-scoped, and payload-free. */
export function describeCallFailure(method: DesktopMethod, error: unknown): string {
  if (error instanceof BridgeFailure) return `${method} failed (${error.kind}): ${error.message}`
  if (error instanceof Error) return `${method} failed: ${error.message}`
  return `${method} failed`
}

function serializedSize(payload: object): number | null {
  try {
    return JSON.stringify(payload).length
  }
  catch {
    return null
  }
}
