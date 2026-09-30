import { describe, expect, test } from 'bun:test'
import { BridgeClient, BridgeFailure, MAX_IN_FLIGHT } from './bridge-client'
import type { BridgeChannel } from './bridge-client'
import { MAX_REQUEST_BYTES } from '../shared/methods'

class FakeChannel implements BridgeChannel {
  readonly lines: string[] = []
  readonly closeReasons: string[] = []
  private readonly messages: Array<(line: string) => void> = []
  private readonly closes: Array<(reason: string) => void> = []
  closed = false

  write(line: string): boolean {
    if (this.closed) return false
    this.lines.push(line)
    return true
  }

  onMessage(listener: (line: string) => void): void {
    this.messages.push(listener)
  }

  onClose(listener: (reason: string) => void): void {
    this.closes.push(listener)
  }

  close(): void {
    this.closed = true
  }

  respond(payload: unknown): void {
    const line = JSON.stringify(payload)
    for (const listener of this.messages) listener(line)
  }

  respondRaw(line: string): void {
    for (const listener of this.messages) listener(line)
  }

  emitClose(reason: string): void {
    for (const listener of this.closes) listener(reason)
  }

  lastRequest(): { id: number, method: string, params: Record<string, unknown> } {
    return JSON.parse(this.lines[this.lines.length - 1]!)
  }
}

function client(channel: FakeChannel, options: { timeoutMs?: number, maxInFlight?: number } = {}) {
  const diagnostics: string[] = []
  const bridge = new BridgeClient(channel, { onDiagnostic: message => diagnostics.push(message), ...options })
  return { bridge, diagnostics }
}

function ok(id: number, result: unknown, protocol = 1) {
  return { id, protocol, ok: true, result }
}

async function failureOf(promise: Promise<unknown>): Promise<BridgeFailure> {
  try {
    await promise
  }
  catch (error) {
    if (error instanceof BridgeFailure) return error
    throw error
  }
  throw new Error('expected the request to fail')
}

describe('bridge client correlation', () => {
  test('matches responses by id and tolerates out-of-order answers', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const first = bridge.request('get_tasks', { request: { limit: 1 } })
    const second = bridge.request('get_status', { request: {} })
    expect(channel.lines).toHaveLength(2)
    expect(channel.lastRequest().method).toBe('get_status')
    const [firstId, secondId] = channel.lines.map(line => JSON.parse(line).id as number)

    channel.respond(ok(secondId!, { branch: null }))
    channel.respond(ok(firstId!, { items: [] }))

    expect(await first).toEqual({ items: [] })
    expect(await second).toEqual({ branch: null })
    expect(bridge.inFlight).toBe(0)
  })

  test('ignores blank lines and answers for unknown ids', async () => {
    const channel = new FakeChannel()
    const { bridge, diagnostics } = client(channel)
    const pending = bridge.request('get_app_metadata')
    const id = channel.lastRequest().id
    channel.respondRaw('')
    channel.respondRaw('   ')
    channel.respond(ok(id + 999, {}))
    expect(diagnostics).toHaveLength(1)
    expect(diagnostics[0]).toContain('unknown request id')
    channel.respond(ok(id, { name: 'phasegent' }))
    expect(await pending).toEqual({ name: 'phasegent' })
  })

  test('turns a backend error envelope into a bounded transport failure', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const pending = bridge.request('get_tasks', { request: { limit: 1 } })
    channel.respond({
      id: channel.lastRequest().id,
      protocol: 1,
      ok: false,
      error: { kind: 'argument', message: 'get_tasks parameter limit is invalid' },
    })
    const failure = await failureOf(pending)
    expect(failure.kind).toBe('transport')
    expect(failure.message).toContain('argument')
    expect(failure.message).toContain('limit is invalid')
  })

  test('never echoes the request payload back in an error message', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const pending = bridge.request('set_credential', {
      request: { role: 'executor', provider: 'redmine', credential: 'super-secret-value' },
    })
    channel.respond({
      id: channel.lastRequest().id,
      protocol: 1,
      ok: false,
      error: { kind: 'backend', message: 'credential store unavailable' },
    })
    const failure = await failureOf(pending)
    expect(failure.message).not.toContain('super-secret-value')
    expect(failure.message).toContain('credential store unavailable')
  })
})

describe('bridge client fencing and lifecycle', () => {
  test('rejects in-flight requests when the protocol version differs', async () => {
    const channel = new FakeChannel()
    const { bridge, diagnostics } = client(channel)
    const pending = failureOf(bridge.request('get_status'))
    channel.respond({ id: channel.lastRequest().id, protocol: 2, ok: true, result: {} })
    const failure = await pending
    expect(failure.kind).toBe('protocol')
    expect(bridge.closed).toBe(true)
    expect(channel.closed).toBe(true)
    expect(diagnostics.some(message => message.includes('protocol'))).toBe(true)
  })

  test('rejects in-flight requests on a malformed response line', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const pending = failureOf(bridge.request('get_status'))
    channel.respondRaw('not json at all')
    const failure = await pending
    expect(failure.kind).toBe('protocol')
    expect(bridge.closed).toBe(true)
  })

  test('times out a request that never gets an answer', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel, { timeoutMs: 10 })
    const failure = await failureOf(bridge.request('get_tasks'))
    expect(failure.kind).toBe('timeout')
    expect(bridge.inFlight).toBe(0)
  })

  test('rejects new requests beyond the in-flight bound', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel, { maxInFlight: 1 })
    const first = bridge.request('get_status')
    const failure = await failureOf(bridge.request('get_tasks'))
    expect(failure.kind).toBe('busy')
    channel.respond(ok(channel.lastRequest().id, {}))
    await first
    expect(MAX_IN_FLIGHT).toBeGreaterThan(0)
  })

  test('rejects a payload above the request bound before writing anything', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const failure = await failureOf(
      bridge.request('set_config_setting', {
        request: { setting: 'x', value: 'v'.repeat(MAX_REQUEST_BYTES) },
      }),
    )
    expect(failure.kind).toBe('argument')
    expect(channel.lines).toHaveLength(0)
  })

  test('rejects pending requests when the backend closes', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const pending = failureOf(bridge.request('get_status'))
    channel.emitClose('desktop bridge exited with code 1')
    const failure = await pending
    expect(failure.kind).toBe('closed')
    expect(failure.message).toContain('code 1')
  })

  test('dispose rejects pending requests, closes the channel, and blocks new ones', async () => {
    const channel = new FakeChannel()
    const { bridge } = client(channel)
    const pending = failureOf(bridge.request('get_status'))
    bridge.dispose()
    const failure = await pending
    expect(failure.kind).toBe('closed')
    expect(channel.closed).toBe(true)
    expect((await failureOf(bridge.request('get_status'))).kind).toBe('closed')
  })

  test('fails immediately when the channel no longer accepts writes', async () => {
    const channel = new FakeChannel()
    channel.closed = true
    const { bridge } = client(channel)
    const failure = await failureOf(bridge.request('get_status'))
    expect(failure.kind).toBe('closed')
  })
})
