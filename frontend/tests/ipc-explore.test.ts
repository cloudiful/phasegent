import { describe, expect, test } from 'bun:test'
import {
  clearRoleCredential,
  fetchConfigSnapshot,
  fetchProvisioningStatus,
  setNonSecretSetting,
  setRoleCredential,
  snapshotEndpointForRole,
  snapshotProviderForRole,
  snapshotRedmineApiBase,
  snapshotRoleEntry,
} from '../src/ipc'
import type { ConfigSnapshotRaw } from '../src/ipc'
import { ROLE_ITEMS, roleHint } from '../src/pages/roleOptions'
import type { RoleId } from '../src/types'

type DesktopApi = NonNullable<Window['phasegent']>

const EXPLORE: RoleId = 'explore'

const SNAPSHOT: ConfigSnapshotRaw = {
  database_path: '/data/phasegent.sqlite3',
  roles: [
    {
      role: 'explore',
      provider: 'redmine',
      redmine_close_status_id: 5,
      redmine_credential: { present: true, length: 40 },
    },
    {
      role: 'executor',
      provider: 'redmine',
      redmine_close_status_id: null,
      redmine_credential: { present: false },
    },
  ],
  global_settings: [
    { name: 'PHASEGENT_REDMINE_API_BASE', present: true, length: 34, sanitized_value: 'https://redmine.example.invalid' },
  ],
  global_default_provider: 'redmine',
}

/** Install a stub preload API; unmocked methods fail loudly. */
function installDesktopApi(overrides: Partial<DesktopApi>): void {
  const unexpected = async (): Promise<never> => {
    throw new Error('unexpected bridge call')
  }
  const api: DesktopApi = {
    getAppMetadata: unexpected,
    getConfigSnapshot: unexpected,
    getBranchContext: unexpected,
    getTasks: unexpected,
    getStatus: unexpected,
    setConfigSetting: unexpected,
    clearConfigSetting: unexpected,
    setCredential: unexpected,
    clearCredential: unexpected,
    getProvisioningStatus: unexpected,
    ...overrides,
  }
  ;(globalThis as unknown as { window?: unknown }).window = { phasegent: api }
}

function removeDesktopApi(): void {
  delete (globalThis as unknown as { window?: unknown }).window
}

describe('role selector inventory', () => {
  test('exposes every provisioned role including explore', () => {
    expect(ROLE_ITEMS.map(item => item.value)).toEqual([
      'admin',
      'orchestrator',
      'executor',
      'reviewer',
      'explore',
    ])
  })

  test('describes explore as the read-only reconnaissance role', () => {
    const hint = roleHint(EXPLORE)
    expect(hint.length).toBeGreaterThan(0)
    expect(hint.toLowerCase()).toContain('recon')
  })

  test('every selector entry carries a hint', () => {
    for (const item of ROLE_ITEMS) expect(roleHint(item.value)).toBe(item.hint)
  })
})

describe('explore through the existing snapshot helpers', () => {
  test('reads the explore role entry, provider, and shared endpoint', () => {
    const entry = snapshotRoleEntry(SNAPSHOT, EXPLORE)
    expect(entry?.role).toBe('explore')
    expect(entry?.redmine_credential).toEqual({ present: true, length: 40 })
    expect(entry?.redmine_close_status_id).toBe(5)
    expect(snapshotProviderForRole(SNAPSHOT, EXPLORE)).toBe('redmine')
    expect(snapshotRedmineApiBase(SNAPSHOT)).toBe('https://redmine.example.invalid')
    expect(snapshotEndpointForRole(SNAPSHOT, EXPLORE)).toBe('https://redmine.example.invalid')
  })

  test('reports no entry for an unprovisioned role without throwing', () => {
    expect(snapshotRoleEntry(SNAPSHOT, 'orchestrator')).toBeNull()
    expect(snapshotEndpointForRole(SNAPSHOT, 'orchestrator')).toBe('')
    expect(snapshotRoleEntry(null, EXPLORE)).toBeNull()
    expect(snapshotProviderForRole(null, EXPLORE)).toBe('')
  })
})

describe('explore through the existing bridge routes', () => {
  test('fetchConfigSnapshot returns an explore entry unchanged', async () => {
    installDesktopApi({ getConfigSnapshot: async () => SNAPSHOT })
    try {
      const result = await fetchConfigSnapshot()
      expect(result.data.roles.map(entry => entry.role)).toEqual(['explore', 'executor'])
      expect(result.data.roles[0]?.redmine_credential.present).toBe(true)
    }
    finally {
      removeDesktopApi()
    }
  })

  test('setting and credential mutations forward the explore role unchanged', async () => {
    const settings: unknown[] = []
    const credentials: unknown[] = []
    installDesktopApi({
      setConfigSetting: async (request) => {
        settings.push(request)
        return { setting: request.setting, role: request.role ?? null, updated: true }
      },
      setCredential: async (request) => {
        credentials.push(request)
        return {
          role: request.role,
          provider: request.provider,
          present: true,
          length: request.credential.length,
          source: 'config',
        }
      },
      clearCredential: async request => ({ role: request.role, provider: request.provider, cleared: true }),
    })
    try {
      await setNonSecretSetting(EXPLORE, 'PHASEGENT_PROVIDER', 'redmine')
      expect(settings).toEqual([{ role: 'explore', setting: 'PHASEGENT_PROVIDER', value: 'redmine' }])

      const token = 'explore-secret-token'
      const presence = await setRoleCredential(EXPLORE, 'redmine', token)
      expect(credentials).toEqual([{ role: 'explore', provider: 'redmine', credential: token }])
      expect(presence).toEqual({ present: true, length: token.length })
      expect(JSON.stringify(presence)).not.toContain(token)
      expect(await clearRoleCredential(EXPLORE, 'redmine')).toBe(true)
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchProvisioningStatus surfaces the provisioned explore identity', async () => {
    installDesktopApi({
      getProvisioningStatus: async () => ({
        role: 'explore',
        user_id: 44,
        login: 'phasegent-explore',
        credential_present: true,
        credential_length: 40,
      }),
    })
    try {
      expect(await fetchProvisioningStatus(EXPLORE)).toEqual({
        role: 'explore',
        user_id: 44,
        login: 'phasegent-explore',
        credential_present: true,
        credential_length: 40,
      })
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchProvisioningStatus stays null for an unprovisioned explore role', async () => {
    installDesktopApi({
      getProvisioningStatus: async () => ({
        role: 'explore',
        user_id: null,
        login: null,
        credential_present: false,
        credential_length: 0,
      }),
    })
    try {
      const status = await fetchProvisioningStatus(EXPLORE)
      expect(status?.login ?? null).toBeNull()
      expect(status?.credential_present).toBe(false)
    }
    finally {
      removeDesktopApi()
    }
  })
})