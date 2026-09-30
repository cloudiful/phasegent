import { describe, expect, test } from 'bun:test'
import { channelFor } from '../shared/methods'
import { createDesktopApi, type DesktopInvoke, type PhasegentDesktopApi } from './api'

const API_METHODS = [
  'getAppMetadata',
  'getConfigSnapshot',
  'getBranchContext',
  'getTasks',
  'getStatus',
  'setConfigSetting',
  'clearConfigSetting',
  'setCredential',
  'clearCredential',
  'getProvisioningStatus',
] as const

function recording() {
  const calls: Array<{ method: string, payload: unknown }> = []
  const invoke: DesktopInvoke = async (method, payload) => {
    calls.push({ method, payload })
    return { ok: true }
  }
  const api: PhasegentDesktopApi = createDesktopApi(invoke)
  return { api, calls }
}

describe('preload API surface', () => {
  test('exposes exactly the documented typed methods', () => {
    const { api } = recording()
    expect(Object.keys(api).sort()).toEqual([...API_METHODS].sort())
    for (const name of API_METHODS) {
      expect(typeof api[name]).toBe('function')
    }
  })

  test('never exposes a raw IPC or channel primitive', () => {
    const { api } = recording()
    for (const forbidden of ['invoke', 'send', 'on', 'ipcRenderer', 'channel', 'require', 'spawn']) {
      expect(Object.keys(api)).not.toContain(forbidden)
    }
  })

  test('maps every call onto its allowlisted method name', async () => {
    const { api, calls } = recording()
    await api.getAppMetadata()
    await api.getConfigSnapshot()
    await api.getBranchContext()
    await api.getTasks()
    await api.getStatus({ role: 'executor' })
    await api.setConfigSetting({ setting: 'a', value: 'b' })
    await api.clearConfigSetting({ setting: 'a' })
    await api.setCredential({ role: 'executor', provider: 'redmine', credential: 'x' })
    await api.clearCredential({ role: 'executor', provider: 'redmine' })
    await api.getProvisioningStatus({ role: 'executor' })
    expect(calls).toEqual([
      { method: 'get_app_metadata', payload: undefined },
      { method: 'get_config_snapshot', payload: undefined },
      { method: 'get_branch_context', payload: undefined },
      { method: 'get_tasks', payload: {} },
      { method: 'get_status', payload: { role: 'executor' } },
      { method: 'set_config_setting', payload: { setting: 'a', value: 'b' } },
      { method: 'clear_config_setting', payload: { setting: 'a' } },
      { method: 'set_credential', payload: { role: 'executor', provider: 'redmine', credential: 'x' } },
      { method: 'clear_credential', payload: { role: 'executor', provider: 'redmine' } },
      { method: 'get_provisioning_status', payload: { role: 'executor' } },
    ])
  })

  test('every method name is also an allowlisted desktop channel', () => {
    for (const name of API_METHODS) {
      const method = name.replace(/[A-Z]/g, letter => `_${letter.toLowerCase()}`)
      expect(channelFor(method as never)).toBe(`phasegent:${method}`)
    }
  })

  test('propagates backend rejections unchanged', async () => {
    const api = createDesktopApi(async () => {
      throw new Error('get_tasks failed (timeout): no answer')
    })
    await expect(api.getTasks()).rejects.toThrow('get_tasks failed (timeout)')
  })
})
