import { describe, expect, test } from 'bun:test'
import { BridgeFailure } from '../bridge/bridge-client'
import { createFakeChild, fakeSpawn, flush } from '../bridge/fake-child'
import { BackendBridge } from './backend'

function bridgeFor(child: ReturnType<typeof createFakeChild>, binary = '/opt/phasegent-backend') {
  const diagnostics: string[] = []
  const { spawn, calls } = fakeSpawn(child)
  const bridge = new BackendBridge({
    resolveBinary: () => binary,
    spawn,
    clientOptions: { timeoutMs: 50 },
    onDiagnostic: message => diagnostics.push(message),
  })
  return { bridge, calls, diagnostics }
}

describe('backend lifecycle', () => {
  test('resolves and spawns the companion on the first request only', async () => {
    const child = createFakeChild()
    const { bridge, calls } = bridgeFor(child)
    const first = bridge.request('get_app_metadata')
    const second = bridge.request('get_branch_context')
    expect(calls).toHaveLength(1)
    expect(bridge.spawnCount).toBe(1)
    expect(bridge.running).toBe(true)
    child.respond('{"id":1,"protocol":1,"ok":true,"result":{"name":"phasegent"}}')
    child.respond('{"id":2,"protocol":1,"ok":true,"result":{"branch":null}}')
    await flush()
    expect(await first).toEqual({ name: 'phasegent' })
    expect(await second).toEqual({ branch: null })
  })

  test('respawns after the companion exits', async () => {
    const child = createFakeChild()
    const { bridge } = bridgeFor(child)
    const rejected = bridge.request<Error>('get_status').catch(error => error as Error)
    child.exit(1)
    await flush()
    expect((await rejected).message).toContain('code 1')
    expect(bridge.running).toBe(false)

    const second = bridge.request('get_status')
    expect(bridge.spawnCount).toBe(2)
    child.respond('{"id":1,"protocol":1,"ok":true,"result":{"connection":"offline"}}')
    await flush()
    expect(await second).toEqual({ connection: 'offline' })
  })

  test('surfaces a missing companion as a bounded failure instead of spawning', async () => {
    const child = createFakeChild()
    const { spawn, calls } = fakeSpawn(child)
    const bridge = new BackendBridge({
      resolveBinary: () => {
        throw new Error('desktop backend not found; build it with cargo build --bin phasegent')
      },
      spawn,
    })
    const failure = await bridge.request('get_status').catch(error => error)
    expect(failure).toBeInstanceOf(BridgeFailure)
    expect((failure as BridgeFailure).kind).toBe('closed')
    expect((failure as BridgeFailure).message).toContain('desktop backend not found')
    expect(calls).toHaveLength(0)
  })

  test('dispose stops the companion and rejects in-flight requests', async () => {
    const child = createFakeChild()
    const { bridge } = bridgeFor(child)
    const pending = bridge.request('get_status')
    bridge.dispose()
    expect(bridge.running).toBe(false)
    await expect(pending).rejects.toThrow('shut down')
  })
})
