import { describe, expect, test } from 'bun:test'
import {
  STALE_AFTER_MS,
  capitalizeProvider,
  clearNonSecretSetting,
  clearRoleCredential,
  desktopErrorMessage,
  fetchConfigSnapshot,
  fetchProvisioningStatus,
  fetchStatus,
  fetchTasks,
  filterTasks,
  formatClock,
  isStale,
  mapIssueStateToTaskStatus,
  mapStatusPayload,
  mapTasksPayload,
  payloadFetchedAtMs,
  setNonSecretSetting,
  setRoleCredential,
  snapshotEndpointForRole,
  snapshotProviderForRole,
  snapshotRoleEntry,
  summarizeTasks,
  testConnection,
  validateEndpoint,
} from '../src/ipc'
import type { ConfigSnapshotRaw, StatusPayloadRaw, TasksPayloadRaw } from '../src/ipc'
import type { TaskItem } from '../src/types'

const SAMPLE_TASKS: TaskItem[] = [
  { id: '#1', title: 'one', phase: 'New', status: 'queued', progress: 0, updatedAt: '2026-09-06T19:00:00Z' },
  { id: '#2', title: 'two', phase: 'In Progress', status: 'running', progress: 10, updatedAt: '2026-09-06T19:01:00Z' },
  { id: '#3', title: 'three', phase: 'Closed', status: 'done', progress: 100, updatedAt: '2026-09-06T19:02:00Z' },
]

describe('ipc view helpers (parity with mock boundary)', () => {
  test('filterTasks returns a copy for all and filters by status', () => {
    const all = filterTasks(SAMPLE_TASKS, 'all')
    expect(all).toHaveLength(3)
    expect(all).not.toBe(SAMPLE_TASKS)
    expect(filterTasks(SAMPLE_TASKS, 'running').map(task => task.id)).toEqual(['#2'])
    expect(filterTasks(SAMPLE_TASKS, 'failed')).toEqual([])
  })

  test('summarizeTasks counts by status and handles empty input', () => {
    expect(summarizeTasks(SAMPLE_TASKS)).toEqual({ queued: 1, running: 1, paused: 0, done: 1, failed: 0 })
    expect(summarizeTasks([])).toEqual({ queued: 0, running: 0, paused: 0, done: 0, failed: 0 })
  })

  test('isStale honors the boundary and the default budget', () => {
    expect(STALE_AFTER_MS).toBe(120_000)
    expect(isStale(0, 120_000, 120_000)).toBe(false)
    expect(isStale(0, 120_001, 120_000)).toBe(true)
    expect(isStale(Date.now(), Date.now())).toBe(false)
  })

  test('formatClock formats valid timestamps and passes invalid input through', () => {
    expect(formatClock('2026-09-06T19:40:00Z')).toMatch(/^\d{2}:\d{2}:\d{2}$/)
    expect(formatClock('bogus')).toBe('bogus')
  })

  test('validateEndpoint rejects blank and non-URL input', () => {
    expect(validateEndpoint('   ')).not.toBeNull()
    expect(validateEndpoint('not-a-url')).not.toBeNull()
    expect(validateEndpoint('https://redmine.example.invalid')).toBeNull()
  })
})

describe('mapIssueStateToTaskStatus', () => {
  test('maps closed variants to done', () => {
    expect(mapIssueStateToTaskStatus('Closed')).toBe('done')
    expect(mapIssueStateToTaskStatus('Resolved')).toBe('done')
    expect(mapIssueStateToTaskStatus('Merged')).toBe('done')
  })

  test('maps failure variants to failed', () => {
    expect(mapIssueStateToTaskStatus('Failed')).toBe('failed')
    expect(mapIssueStateToTaskStatus('Blocked')).toBe('failed')
  })

  test('maps active variants to running and waiting variants to paused', () => {
    expect(mapIssueStateToTaskStatus('In Progress')).toBe('running')
    expect(mapIssueStateToTaskStatus('In Review')).toBe('running')
    expect(mapIssueStateToTaskStatus('On Hold')).toBe('paused')
    expect(mapIssueStateToTaskStatus('Waiting')).toBe('paused')
  })

  test('falls back to queued for unknown states', () => {
    expect(mapIssueStateToTaskStatus('New')).toBe('queued')
    expect(mapIssueStateToTaskStatus('')).toBe('queued')
  })
})

describe('capitalizeProvider / payloadFetchedAtMs / desktopErrorMessage', () => {
  test('capitalizes known providers case-insensitively and passes others through', () => {
    expect(capitalizeProvider('redmine')).toBe('Redmine')
    expect(capitalizeProvider('REDMINE')).toBe('Redmine')
    expect(capitalizeProvider('gitlab')).toBe('GitLab')
    expect(capitalizeProvider('forgejo')).toBe('Forgejo')
    expect(capitalizeProvider('custom')).toBe('custom')
  })

  test('converts positive backend seconds to milliseconds and falls back to now', () => {
    expect(payloadFetchedAtMs(1_700_000_000)).toBe(1_700_000_000_000)
    const before = Date.now()
    const fallback = payloadFetchedAtMs(0)
    expect(fallback).toBeGreaterThanOrEqual(before)
    expect(fallback).toBeLessThanOrEqual(Date.now())
  })

  test('redacts error values without leaking structure', () => {
    expect(desktopErrorMessage(new Error('boom'))).toBe('boom')
    expect(desktopErrorMessage('plain')).toBe('plain')
    expect(desktopErrorMessage({ code: 1 })).toBe(JSON.stringify({ code: 1 }))
  })
})

describe('mapTasksPayload', () => {
  const payload: TasksPayloadRaw = {
    branch: 'feature/x',
    bound_issue: 149,
    provider: 'redmine',
    role: 'executor',
    items: [
      { number: 1, title: 'open work', state: 'New' },
      { number: 2, title: 'finished work', state: 'Closed' },
    ],
    has_more: false,
    data_source: 'provider',
    fetched_at: 1_700_000_000,
    warning: 'slow backend',
  }

  test('maps entries to view items with stable ids and progress', () => {
    const result = mapTasksPayload(payload)
    expect(result.fetchedAt).toBe(1_700_000_000_000)
    expect(result.data.branch).toBe('feature/x')
    expect(result.data.boundIssue).toBe(149)
    expect(result.data.warning).toBe('slow backend')
    expect(result.data.items).toHaveLength(2)
    expect(result.data.items[0]).toMatchObject({ id: '#1', title: 'open work', phase: 'New', status: 'queued', progress: 0 })
    expect(result.data.items[1]).toMatchObject({ id: '#2', status: 'done', progress: 100 })
    expect(new Date(result.data.items[0].updatedAt).getTime()).toBe(1_700_000_000_000)
  })

  test('defaults missing warning to null', () => {
    const result = mapTasksPayload({ ...payload, warning: undefined, items: [] })
    expect(result.data.items).toEqual([])
    expect(result.data.warning).toBeNull()
  })
})

describe('mapStatusPayload', () => {
  const payload: StatusPayloadRaw = {
    branch: 'main',
    bound_issue: 149,
    bound_issue_title: 'GUI work',
    bound_issue_state: 'In Progress',
    provider: 'redmine',
    role: 'executor',
    endpoint: 'https://redmine.example.invalid',
    connection: 'connected',
    running_timers: 2,
    recent_timers: [
      { run_id: 'run-1', issue: 149, phase: 'release-ci', role: 'executor', status: 'running', sync_status: 'ok', started_at: 1_700_000_000, finished_at: null },
      { run_id: 'run-2', issue: 148, phase: 'done', role: 'executor', status: 'finished', sync_status: 'failed', started_at: 1_699_999_000, finished_at: 1_699_999_100 },
    ],
    fetched_at: 1_700_000_100,
    warning: 'degraded note',
    statuses_unsupported: null,
  }

  test('normalizes connection, provider, totals, and recent ordering', () => {
    const result = mapStatusPayload(payload)
    expect(result.fetchedAt).toBe(1_700_000_100_000)
    expect(result.data.summary.connection).toBe('connected')
    expect(result.data.summary.provider).toBe('Redmine')
    expect(result.data.summary.endpoint).toBe('https://redmine.example.invalid')
    expect(result.data.summary.totals.running).toBe(2)
    expect(result.data.warning).toBe('degraded note')
    // Bound-issue notice is newest, then backend warning, then timers.
    expect(result.data.summary.recent[0].id).toBe('bound-149')
    expect(result.data.summary.recent[1].id).toBe('warn-backend')
    expect(result.data.summary.recent.find(entry => entry.id === 'run-1')?.level).toBe('info')
    expect(result.data.summary.recent.find(entry => entry.id === 'run-2')?.level).toBe('error')
  })

  test('treats unknown connections as offline and missing endpoint as empty', () => {
    const result = mapStatusPayload({ ...payload, connection: 'weird', endpoint: null, warning: null, bound_issue: null })
    expect(result.data.summary.connection).toBe('offline')
    expect(result.data.summary.endpoint).toBe('')
    expect(result.data.boundIssue).toBeNull()
    expect(result.data.warning).toBeNull()
  })

  test('surfaces capability notices before timers', () => {
    const result = mapStatusPayload({ ...payload, bound_issue: null, warning: null, statuses_unsupported: 'statuses unavailable' })
    expect(result.data.unsupported).toBe('statuses unavailable')
    expect(result.data.summary.recent[0].id).toBe('unsupported-status')
  })
})

describe('snapshot helpers', () => {
  const snapshot: ConfigSnapshotRaw = {
    database_path: '/tmp/phasegent.db',
    roles: [
      {
        role: 'executor',
        provider: 'redmine',
        forgejo_api_base: null,
        forgejo_repository: null,
        redmine_api_base: 'https://redmine.example.invalid',
        redmine_close_status_id: 5,
        gitlab_api_base: null,
        forgejo_credential: { present: false, length: 0 },
        redmine_credential: { present: true, length: 40 },
        gitlab_credential: { present: false },
      },
    ],
    global_settings: [],
    global_default_provider: 'forgejo',
  }

  test('finds role entries and prefers role provider over the global default', () => {
    expect(snapshotRoleEntry(null, 'executor')).toBeNull()
    expect(snapshotRoleEntry(snapshot, 'missing')).toBeNull()
    expect(snapshotRoleEntry(snapshot, 'executor')?.role).toBe('executor')
    expect(snapshotProviderForRole(snapshot, 'executor')).toBe('redmine')
    expect(snapshotProviderForRole(snapshot, 'missing')).toBe('forgejo')
    expect(snapshotProviderForRole(null, 'executor')).toBe('')
  })

  test('resolves endpoints with redmine priority and empty fallback', () => {
    expect(snapshotEndpointForRole(snapshot, 'executor')).toBe('https://redmine.example.invalid')
    expect(snapshotEndpointForRole(snapshot, 'missing')).toBe('')
    expect(snapshotEndpointForRole(null, 'executor')).toBe('')
  })
})

describe('preload-unavailable fallback (plain browser / Bun has no window)', () => {
  test('fetchTasks returns safe empty state instead of throwing', async () => {
    const result = await fetchTasks()
    expect(result.data.items).toEqual([])
    expect(result.data.warning).toBeNull()
    expect(typeof result.fetchedAt).toBe('number')
  })

  test('fetchStatus returns offline empty state instead of throwing', async () => {
    const result = await fetchStatus()
    expect(result.data.summary.connection).toBe('offline')
    expect(result.data.summary.recent).toEqual([])
    expect(result.data.branch).toBeNull()
  })

  test('fetchConfigSnapshot returns empty snapshot instead of throwing', async () => {
    const result = await fetchConfigSnapshot()
    expect(result.data.roles).toEqual([])
    expect(typeof result.fetchedAt).toBe('number')
  })

  test('testConnection validates locally and previews success without a backend', async () => {
    const invalid = await testConnection('   ')
    expect(invalid.ok).toBe(false)
    const preview = await testConnection('https://redmine.example.invalid')
    expect(preview.ok).toBe(true)
    expect(preview.message).toContain('browser preview')
  })

  test('mutations reject with a bounded message instead of a stack trace', async () => {
    await expect(setNonSecretSetting(null, 'PHASEGENT_PROVIDER', 'forgejo')).rejects.toThrow('Desktop backend is unavailable in this preview.')
    await expect(clearNonSecretSetting('executor', 'PHASEGENT_API_BASE')).rejects.toThrow('Desktop backend is unavailable in this preview.')
    await expect(setRoleCredential('executor', 'redmine', 'secret')).rejects.toThrow('Desktop backend is unavailable in this preview.')
    await expect(clearRoleCredential('executor', 'redmine')).rejects.toThrow('Desktop backend is unavailable in this preview.')
    expect(await fetchProvisioningStatus('executor')).toBeNull()
  })
})

// ---------------------------------------------------------------------------
// Preload bridge (`window.phasegent`), the renderer's only backend surface.
// ---------------------------------------------------------------------------

type DesktopApi = NonNullable<Window['phasegent']>

const BRIDGE_TASKS: TasksPayloadRaw = {
  branch: 'feat/652',
  bound_issue: 652,
  provider: 'redmine',
  role: 'executor',
  items: [{ number: 652, title: 'Migrate desktop shell to Electron', state: 'In Progress' }],
  has_more: false,
  data_source: 'provider',
  fetched_at: 1_700_000_000,
  warning: null,
}

const BRIDGE_STATUS: StatusPayloadRaw = {
  branch: 'feat/652',
  bound_issue: 652,
  bound_issue_title: 'Migrate desktop shell to Electron',
  bound_issue_state: 'In Progress',
  provider: 'redmine',
  role: 'executor',
  endpoint: 'https://redmine.example.invalid',
  connection: 'connected',
  running_timers: 0,
  recent_timers: [],
  fetched_at: 1_700_000_000,
  warning: null,
  statuses_unsupported: null,
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

describe('preload bridge (window.phasegent)', () => {
  test('fetchTasks sends the unchanged open-task request and maps the payload', async () => {
    let seen: unknown = 'unset'
    installDesktopApi({
      getTasks: async (request) => {
        seen = request
        return BRIDGE_TASKS
      },
    })
    try {
      const result = await fetchTasks()
      expect(seen).toEqual({ limit: 20, state: 'open' })
      expect(result.fetchedAt).toBe(1_700_000_000_000)
      expect(result.data.items[0]).toMatchObject({ id: '#652', title: 'Migrate desktop shell to Electron', status: 'running' })
      expect(result.data.branch).toBe('feat/652')
      expect(result.data.boundIssue).toBe(652)
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchTasks surfaces the bounded bridge error instead of empty state', async () => {
    installDesktopApi({
      getTasks: async () => {
        throw new Error('get_tasks failed (backend): provider credential missing')
      },
    })
    try {
      await expect(fetchTasks()).rejects.toThrow('get_tasks failed (backend): provider credential missing')
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchStatus probes with an empty request and maps the payload', async () => {
    let seen: unknown = 'unset'
    installDesktopApi({
      getStatus: async (request) => {
        seen = request
        return BRIDGE_STATUS
      },
    })
    try {
      const result = await fetchStatus()
      expect(seen).toEqual({})
      expect(result.data.summary.connection).toBe('connected')
      expect(result.data.summary.provider).toBe('Redmine')
      expect(result.data.boundIssue).toBe(652)
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchConfigSnapshot returns the redacted snapshot', async () => {
    installDesktopApi({
      getConfigSnapshot: async () => ({
        database_path: '/data/phasegent.sqlite3',
        roles: [],
        global_settings: [],
        global_default_provider: 'redmine',
      }),
    })
    try {
      const result = await fetchConfigSnapshot()
      expect(result.data.database_path).toBe('/data/phasegent.sqlite3')
      expect(result.data.global_default_provider).toBe('redmine')
      expect(result.data.roles).toEqual([])
    }
    finally {
      removeDesktopApi()
    }
  })

  test('setting and credential mutations forward the existing parameter shape', async () => {
    const settings: unknown[] = []
    const credentials: unknown[] = []
    installDesktopApi({
      setConfigSetting: async (request) => {
        settings.push(request)
        return { setting: request.setting, role: request.role ?? null, updated: true }
      },
      clearConfigSetting: async request => ({ setting: request.setting, role: request.role ?? null, cleared: true }),
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
      await setNonSecretSetting('  ', 'PHASEGENT_PROVIDER', 'forgejo')
      await setNonSecretSetting('executor', 'PHASEGENT_API_BASE', 'https://redmine.example.invalid')
      expect(settings).toEqual([
        { role: null, setting: 'PHASEGENT_PROVIDER', value: 'forgejo' },
        { role: 'executor', setting: 'PHASEGENT_API_BASE', value: 'https://redmine.example.invalid' },
      ])
      expect(await clearNonSecretSetting('executor', 'PHASEGENT_API_BASE')).toBe(true)

      const token = 'super-secret-token'
      const presence = await setRoleCredential('executor', 'redmine', token)
      expect(credentials).toEqual([{ role: 'executor', provider: 'redmine', credential: token }])
      expect(presence).toEqual({ present: true, length: token.length })
      expect(JSON.stringify(presence)).not.toContain(token)
      expect(await clearRoleCredential('executor', 'redmine')).toBe(true)
    }
    finally {
      removeDesktopApi()
    }
  })

  test('fetchProvisioningStatus returns presence metadata and null when the bridge fails', async () => {
    installDesktopApi({
      getProvisioningStatus: async () => ({
        role: 'executor',
        user_id: 12,
        login: 'agent-executor',
        credential_present: true,
        credential_length: 40,
      }),
    })
    try {
      expect(await fetchProvisioningStatus('executor')).toMatchObject({
        role: 'executor',
        login: 'agent-executor',
        credential_present: true,
        credential_length: 40,
      })
    }
    finally {
      removeDesktopApi()
    }

    installDesktopApi({
      getProvisioningStatus: async () => {
        throw new Error('get_provisioning_status failed (backend): unavailable')
      },
    })
    try {
      expect(await fetchProvisioningStatus('executor')).toBeNull()
    }
    finally {
      removeDesktopApi()
    }
  })

  test('testConnection probes the backend and reports a bounded failure', async () => {
    let probes = 0
    installDesktopApi({
      getStatus: async () => {
        probes += 1
        return BRIDGE_STATUS
      },
    })
    try {
      expect(await testConnection('https://redmine.example.invalid')).toEqual({
        ok: true,
        message: 'Connection check passed for https://redmine.example.invalid.',
      })
      expect(probes).toBe(1)
    }
    finally {
      removeDesktopApi()
    }

    installDesktopApi({
      getStatus: async () => {
        throw new Error('get_status failed (backend): provider credential missing')
      },
    })
    try {
      const failed = await testConnection('https://redmine.example.invalid')
      expect(failed.ok).toBe(false)
      expect(failed.message).toContain('credential missing')
    }
    finally {
      removeDesktopApi()
    }
  })
})
