// Local typed stand-ins for the phase-3 Tauri IPC backend.
// Pages treat these loaders as the data boundary: same shapes, same
// async contract. No credentials or secrets are stored or returned here.

import type {
  SettingsState,
  StatusSummary,
  TaskItem,
  TaskStatus,
} from './types'

export const STALE_AFTER_MS = 120_000
const LATENCY_MS = 350
const SETTINGS_KEY = 'phasegent.settings.v1'

export const TASK_STATUSES: TaskStatus[] = ['queued', 'running', 'paused', 'done', 'failed']

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  queued: 'Queued',
  running: 'Running',
  paused: 'Paused',
  done: 'Done',
  failed: 'Failed',
}

export const MOCK_TASKS: TaskItem[] = [
  { id: 'PGT-1042', title: 'Sync provider issues for milestone 3', phase: 'Collect', status: 'running', progress: 62, updatedAt: '2026-09-06T19:32:10Z' },
  { id: 'PGT-1041', title: 'Reconcile phase checkpoints', phase: 'Review', status: 'queued', progress: 0, updatedAt: '2026-09-06T19:28:44Z' },
  { id: 'PGT-1039', title: 'Publish release notes draft', phase: 'Release', status: 'paused', progress: 40, updatedAt: '2026-09-06T18:51:02Z' },
  { id: 'PGT-1036', title: 'Rotate local provider token reference', phase: 'Maintain', status: 'done', progress: 100, updatedAt: '2026-09-06T17:12:37Z' },
  { id: 'PGT-1031', title: 'Backfill status snapshots', phase: 'Collect', status: 'failed', progress: 78, updatedAt: '2026-09-06T16:44:19Z' },
  { id: 'PGT-1028', title: 'Verify Windows artifact manifest', phase: 'Release', status: 'done', progress: 100, updatedAt: '2026-09-06T15:03:55Z' },
  { id: 'PGT-1024', title: 'Prune stale workflow timers', phase: 'Maintain', status: 'queued', progress: 0, updatedAt: '2026-09-06T14:20:08Z' },
]

export const MOCK_STATUS: StatusSummary = {
  connection: 'connected',
  provider: 'Redmine',
  endpoint: 'https://redmine.example.invalid',
  lastSyncAt: '2026-09-06T19:40:00Z',
  totals: { queued: 2, running: 1, paused: 1, done: 2, failed: 1 },
  recent: [
    { id: 'ev-301', at: '2026-09-06T19:40:00Z', level: 'success', message: 'Issue index refreshed (7 tasks).' },
    { id: 'ev-300', at: '2026-09-06T19:32:10Z', level: 'info', message: 'PGT-1042 entered Collect phase.' },
    { id: 'ev-299', at: '2026-09-06T18:51:02Z', level: 'warning', message: 'PGT-1039 paused by operator.' },
    { id: 'ev-298', at: '2026-09-06T16:44:19Z', level: 'error', message: 'PGT-1031 snapshot backfill failed; retry scheduled.' },
  ],
}

export const DEFAULT_SETTINGS: SettingsState = {
  role: 'executor',
  provider: 'redmine',
  endpoint: 'https://redmine.example.invalid',
}

export function filterTasks(tasks: TaskItem[], status: TaskStatus | 'all'): TaskItem[] {
  if (status === 'all') return [...tasks]
  return tasks.filter(task => task.status === status)
}

export function summarizeTasks(tasks: TaskItem[]): Record<TaskStatus, number> {
  const totals: Record<TaskStatus, number> = { queued: 0, running: 0, paused: 0, done: 0, failed: 0 }
  for (const task of tasks) totals[task.status] += 1
  return totals
}

export function isStale(fetchedAtMs: number, nowMs: number, maxAgeMs = STALE_AFTER_MS): boolean {
  return nowMs - fetchedAtMs > maxAgeMs
}

export function formatClock(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

/** Null when the endpoint is usable; otherwise a short reason. */
export function validateEndpoint(endpoint: string): string | null {
  const value = endpoint.trim()
  if (value.length === 0) return 'Enter a provider URL before testing the connection.'
  if (!/^https?:\/\/.+\..+/.test(value)) return 'Provider URL must start with http(s) and include a host.'
  return null
}

function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

export interface FetchResult<T> {
  data: T
  fetchedAt: number
}

export async function fetchTasks(): Promise<FetchResult<TaskItem[]>> {
  await delay(LATENCY_MS)
  return { data: MOCK_TASKS.map(task => ({ ...task })), fetchedAt: Date.now() }
}

export async function fetchStatus(): Promise<FetchResult<StatusSummary>> {
  await delay(LATENCY_MS)
  return {
    data: { ...MOCK_STATUS, totals: { ...MOCK_STATUS.totals }, recent: MOCK_STATUS.recent.map(e => ({ ...e })) },
    fetchedAt: Date.now(),
  }
}

export async function testConnection(endpoint: string): Promise<{ ok: boolean, message: string }> {
  await delay(LATENCY_MS)
  const problem = validateEndpoint(endpoint)
  if (problem) return { ok: false, message: problem }
  return { ok: true, message: `Connection check passed for ${endpoint.trim()}.` }
}

export function loadSettings(): SettingsState {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (!raw) return { ...DEFAULT_SETTINGS }
    const parsed = JSON.parse(raw) as Partial<SettingsState>
    return { ...DEFAULT_SETTINGS, ...parsed }
  }
  catch {
    return { ...DEFAULT_SETTINGS }
  }
}

export function saveSettings(settings: SettingsState): void {
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings))
}
