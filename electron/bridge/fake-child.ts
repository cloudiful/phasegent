// Reusable fake child process for transport and lifecycle tests.

import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { BridgeChildProcess, SpawnFn } from './child-transport'

export interface FakeChild {
  process: BridgeChildProcess
  /** Bytes written to the backend's stdin. */
  stdin: string
  /** Backend stdout; write JSON lines here as responses. */
  respond(line: string): void
  /** Backend stderr diagnostics. */
  diagnose(line: string): void
  /** Close stdout as a clean EOF. */
  endOutput(): void
  /** Emit the process exit event. */
  exit(code: number | null, signal?: string | null): void
  /** Emit a spawn failure. */
  fail(error: Error): void
  /** True after `kill` was called. */
  readonly killed: boolean
}

export function createFakeChild(): FakeChild {
  const errors = new EventEmitter()
  const exits = new EventEmitter()
  const stdin = new PassThrough()
  const stdout = new PassThrough()
  const stderr = new PassThrough()
  stdout.setEncoding('utf8')
  stderr.setEncoding('utf8')
  let written = ''
  let killed = false
  stdin.on('data', (chunk: string) => {
    written += String(chunk)
  })

  const process: BridgeChildProcess = {
    stdin,
    stdout,
    stderr,
    onError: listener => {
      errors.on('error', listener)
    },
    onExit: listener => {
      exits.on('exit', listener)
    },
    kill: () => {
      killed = true
    },
  }

  return {
    process,
    get stdin() {
      return written
    },
    respond: line => {
      stdout.write(`${line}\n`)
    },
    diagnose: line => {
      stderr.write(`${line}\n`)
    },
    endOutput: () => {
      stdout.end()
    },
    exit: (code, signal = null) => {
      exits.emit('exit', code, signal)
    },
    fail: error => {
      errors.emit('error', error)
    },
    get killed() {
      return killed
    },
  }
}

/** Spawn function that records every invocation and returns `child`. */
export function fakeSpawn(child: FakeChild): { spawn: SpawnFn, calls: Array<{ command: string, args: readonly string[] }> } {
  const calls: Array<{ command: string, args: readonly string[] }> = []
  const spawn: SpawnFn = (command, args) => {
    calls.push({ command, args })
    return child.process
  }
  return { spawn, calls }
}

/** Let pending stream events settle before asserting on their effects. */
export async function flush(rounds = 2): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    await new Promise(resolve => setTimeout(resolve, 0))
  }
}
