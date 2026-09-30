import { describe, expect, test } from 'bun:test'
import { BridgeFailure } from '../bridge/bridge-client'
import { DESKTOP_CHANNELS, MAX_PAYLOAD_BYTES, type DesktopMethod } from '../shared/methods'
import { buildParams, registerDesktopIpc, type DesktopInvoker, type IpcMainLike } from './ipc'

interface RecordedCall {
  method: DesktopMethod
  params: Record<string, unknown>
}

function fakeIpc(calls: RecordedCall[], failure?: unknown) {
  const handlers = new Map<string, (event: unknown, payload?: unknown) => unknown>()
  const ipcMain: IpcMainLike = {
    handle: (channel, listener) => {
      handlers.set(channel, listener)
    },
  }
  const errors: string[] = []
  const bridge: DesktopInvoker = {
    request: async <T,>(method: DesktopMethod, params?: Record<string, unknown>): Promise<T> => {
      if (failure !== undefined) throw failure
      calls.push({ method, params: params ?? {} })
      return { method } as unknown as T
    },
  }
  registerDesktopIpc({
    ipcMain,
    bridge,
    onError: message => errors.push(message),
  })
  return {
    channels: [...handlers.keys()],
    invoke: async (channel: string, payload?: unknown) => handlers.get(channel)!({}, payload),
    has: (channel: string) => handlers.has(channel),
    errors,
  }
}

describe('desktop IPC registration', () => {
  test('registers exactly the allowlisted channels', () => {
    const fake = fakeIpc([])
    expect(fake.channels).toEqual([...DESKTOP_CHANNELS])
    expect(fake.has('phasegent:get_issue')).toBe(false)
    expect(fake.has('phasegent:set_credential')).toBe(true)
  })

  test('wraps the payload in the parameter field the backend decodes', async () => {
    const calls: RecordedCall[] = []
    const fake = fakeIpc(calls)
    await fake.invoke('phasegent:get_tasks', { limit: 5, state: 'open' })
    await fake.invoke('phasegent:get_provisioning_status', { role: 'executor' })
    await fake.invoke('phasegent:get_status')
    expect(calls).toEqual([
      { method: 'get_tasks', params: { request: { limit: 5, state: 'open' } } },
      { method: 'get_provisioning_status', params: { query: { role: 'executor' } } },
      { method: 'get_status', params: { request: {} } },
    ])
  })

  test('parameterless methods accept no payload and reject one', async () => {
    const calls: RecordedCall[] = []
    const fake = fakeIpc(calls)
    await fake.invoke('phasegent:get_app_metadata')
    await fake.invoke('phasegent:get_config_snapshot', {})
    expect(calls.map(call => call.params)).toEqual([{}, {}])
    await expect(fake.invoke('phasegent:get_branch_context', { role: 'admin' })).rejects.toThrow(
      'takes no parameters',
    )
  })

  test('rejects non-object and oversized payloads before reaching the bridge', async () => {
    const calls: RecordedCall[] = []
    const fake = fakeIpc(calls)
    await expect(fake.invoke('phasegent:get_tasks', ['nope'])).rejects.toThrow('object payload')
    await expect(
      fake.invoke('phasegent:get_tasks', { state: 'x'.repeat(MAX_PAYLOAD_BYTES) }),
    ).rejects.toThrow('exceeds')
    expect(calls).toHaveLength(0)
  })

  test('surfaces bridge failures without echoing the payload', async () => {
    const fake = fakeIpc([], new BridgeFailure('transport', 'backend: provider token rejected'))
    const failure = await fake.invoke('phasegent:set_credential', { credential: 'top-secret' }).catch(
      error => error as Error,
    )
    expect((failure as Error).message).toContain('set_credential failed (transport)')
    expect((failure as Error).message).toContain('provider token rejected')
    expect((failure as Error).message).not.toContain('top-secret')
    expect(fake.errors).toHaveLength(1)
  })
})

describe('buildParams', () => {
  test('rejects an unknown method even when called directly', () => {
    expect(() => buildParams('issue' as DesktopMethod, {})).toThrow('unsupported desktop method')
  })

  test('accepts an inline credential request unchanged', () => {
    expect(buildParams('set_credential', { role: 'executor', provider: 'redmine', credential: 'x' }))
      .toEqual({ request: { role: 'executor', provider: 'redmine', credential: 'x' } })
  })
})
