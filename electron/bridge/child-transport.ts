// Child-process wiring for the Rust companion's stdio bridge.
//
// Owns spawning, stdout framing, stderr diagnostics, exit/error reporting, and
// graceful shutdown escalation. The correlation logic lives in
// `bridge-client.ts`; the `spawn` function and the child surface are injectable
// so the transport can be exercised without starting a real process.

import { spawn } from 'node:child_process'
import type { Readable, Writable } from 'node:stream'
import type { BridgeChannel } from './bridge-client'
import { MAX_LINE_BYTES, createLineDecoder } from './ndjson'

/** Diagnostics are short JSON lines; anything longer is truncated. */
export const MAX_DIAGNOSTIC_BYTES = 4 * 1024

/**
 * Hidden stdio mode the companion is started with; mirrors `COMMAND` in
 * `src/desktop_bridge.rs` and is the only backend mode this app uses.
 */
export const DEFAULT_BACKEND_ARGS: readonly string[] = ['desktop-bridge']

/** How long the backend may take to drain and exit after stdin ends. */
export const SHUTDOWN_GRACE_MS = 2_000

/** Narrow view of the spawned process so tests can supply a fake. */
export interface BridgeChildProcess {
  stdin: Writable | null
  stdout: Readable | null
  stderr: Readable | null
  onError(listener: (error: Error) => void): void
  onExit(listener: (code: number | null, signal: string | null) => void): void
  kill(signal?: string): void
}

export interface SpawnOptions {
  cwd?: string
  env?: NodeJS.ProcessEnv
}

export type SpawnFn = (
  command: string,
  args: readonly string[],
  options: SpawnOptions,
) => BridgeChildProcess

/** Default spawn: stdio pipes only, no shell, no console window on Windows. */
export const nodeSpawn: SpawnFn = (command, args, options) => {
  const child = spawn(command, [...args], {
    cwd: options.cwd,
    env: options.env,
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  })
  return {
    stdin: child.stdin,
    stdout: child.stdout,
    stderr: child.stderr,
    onError: listener => {
      child.on('error', listener)
    },
    onExit: listener => {
      child.on('exit', (code, signal) => listener(code, signal))
    },
    kill: signal => {
      child.kill(signal as NodeJS.Signals | undefined)
    },
  }
}

export interface ChildChannelOptions {
  binary: string
  args?: readonly string[]
  cwd?: string
  env?: NodeJS.ProcessEnv
  spawn?: SpawnFn
  onDiagnostic?: (message: string) => void
  shutdownGraceMs?: number
}

/**
 * Spawn the backend and return a channel that frames stdout lines and reports
 * every abnormal end through `onClose`. Nothing is written until the caller
 * sends a request.
 */
export function spawnBridgeChannel(options: ChildChannelOptions): BridgeChannel {
  const spawnProcess = options.spawn ?? nodeSpawn
  const onDiagnostic = options.onDiagnostic ?? (() => {})
  const grace = options.shutdownGraceMs ?? SHUTDOWN_GRACE_MS
  const child = spawnProcess(options.binary, options.args ?? DEFAULT_BACKEND_ARGS, {
    cwd: options.cwd,
    env: options.env,
  })

  const messages: Array<(line: string) => void> = []
  const closes: Array<(reason: string) => void> = []
  const stdout = createLineDecoder(MAX_LINE_BYTES)
  const stderr = createLineDecoder(MAX_DIAGNOSTIC_BYTES)
  let draining = true
  let closed = false
  let killTimer: ReturnType<typeof setTimeout> | undefined

  const emitMessages = (lines: string[]) => {
    for (const line of lines) {
      for (const listener of messages) listener(line)
    }
  }

  const finish = (reason: string) => {
    if (closed) return
    closed = true
    draining = false
    if (killTimer !== undefined) clearTimeout(killTimer)
    emitMessages(stdout.end())
    for (const listener of closes) listener(reason)
  }

  child.stdout?.setEncoding('utf8')
  child.stdout?.on('data', (chunk: string) => {
    emitMessages(stdout.push(String(chunk)))
  })
  child.stdout?.on('end', () => finish('desktop bridge closed its output'))
  child.stderr?.setEncoding('utf8')
  child.stderr?.on('data', (chunk: string) => {
    for (const line of stderr.push(String(chunk))) {
      if (line.trim().length > 0) onDiagnostic(line)
    }
  })
  child.onError(error => finish(`could not start the desktop bridge: ${error.message}`))
  child.onExit((code, signal) => {
    const how = signal !== null && signal !== undefined ? `signal ${signal}` : `code ${code}`
    finish(`desktop bridge exited with ${how}`)
  })

  return {
    write(line: string): boolean {
      if (closed || !draining || !child.stdin) return false
      child.stdin.write(`${line}\n`)
      return true
    },
    onMessage(listener) {
      messages.push(listener)
    },
    onClose(listener) {
      closes.push(listener)
      if (closed) listener('desktop bridge is not running')
    },
    close() {
      if (closed || !draining) return
      draining = false
      // EOF is the bridge's graceful stop: it drains in-flight requests,
      // flushes stdout, and exits 0. A backend that ignores it is killed.
      child.stdin?.end()
      killTimer = setTimeout(() => {
        if (!closed) {
          onDiagnostic(`desktop bridge did not exit within ${grace} ms; killing it`)
          child.kill()
        }
      }, grace)
      killTimer.unref?.()
    },
  }
}
