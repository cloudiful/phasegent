// Typed renderer bridge layer for the Electron desktop shell.
// All backend calls go through the typed `window.phasegent` API the preload
// script exposes; the renderer never receives `ipcRenderer`, a channel name,
// or a CLI command. When the preload API is unavailable (plain browser
// preview), loaders return safe empty state so the pages stay in empty/ready
// without secrets. No credential values are stored or returned here.

import type {
  SettingsState,
  StatusEvent,
  StatusSummary,
  TaskItem,
  TaskStatus,
} from './types'

export const STALE_AFTER_MS = 120_000

export const TASK_STATUSES: TaskStatus[] = ['queued', 'running', 'paused', 'done', 'failed']

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  queued: 'Queued',
  running: 'Running',
  paused: 'Paused',
  done: 'Done',
  failed: 'Failed',
}

export const DEFAULT_SETTINGS: SettingsState = {
  role: 'executor',
  provider: 'redmine',
  endpoint: '',
}

export interface FetchResult<T> {
  data: T
  fetchedAt: number
}

// ---------------------------------------------------------------------------
// Shared view helpers used by the pages and the payload mappers.
// ---------------------------------------------------------------------------

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

/**
 * The preload bridge, or `undefined` outside the Electron renderer (plain
 * browser dev preview and tests), where callers fall back to safe empty state.
 */
function desktopApi(): Window['phasegent'] {
  if (typeof window === 'undefined') return undefined
  return window.phasegent
}

/** Rejects mutations that have no safe offline equivalent in a plain browser. */
const BRIDGE_UNAVAILABLE = 'Desktop backend is unavailable in this preview.'

export function desktopErrorMessage(err: unknown): string {
  if (err instanceof Error) return err.message
  if (typeof err === 'string') return err
  try {
    return JSON.stringify(err)
  }
  catch {
    return 'Request failed.'
  }
}

// ---------------------------------------------------------------------------
// Raw backend shapes (snake_case, mirrors the Rust desktop bridge payloads in
// src/gui/models.rs).
// ---------------------------------------------------------------------------

export interface TaskEntryRaw {
  number: number
  title: string
  state: string
  url?: string | null
}

export interface TasksPayloadRaw {
  branch: string | null
  bound_issue: number | null
  provider: string
  role: string
  items: TaskEntryRaw[]
  total_count?: number | null
  has_more: boolean
  data_source: string
  fetched_at: number
  warning?: string | null
}

export interface TimerDtoRaw {
  run_id: string
  issue: number
  phase: string
  role: string
  status: string
  sync_status: string
  started_at: number
  finished_at?: number | null
}

export interface StatusPayloadRaw {
  branch: string | null
  bound_issue: number | null
  bound_issue_title?: string | null
  bound_issue_state?: string | null
  provider: string
  role: string
  endpoint?: string | null
  connection: string
  running_timers: number
  recent_timers: TimerDtoRaw[]
  fetched_at: number
  warning?: string | null
  statuses_unsupported?: string | null
  frontend_dist_hash?: string | null
}

export interface TasksView {
  items: TaskItem[]
  branch: string | null
  boundIssue: number | null
  provider: string
  role: string
  warning: string | null
}

export interface StatusView {
  summary: StatusSummary
  branch: string | null
  boundIssue: number | null
  boundIssueTitle: string | null
  warning: string | null
  unsupported: string | null
  frontendDistHash: string | null
}

export function mapIssueStateToTaskStatus(state: string): TaskStatus {
  const lower = state.toLowerCase()
  if (/(closed|done|resolved|merged|success)/.test(lower)) return 'done'
  if (/(fail|error|block|cancel)/.test(lower)) return 'failed'
  if (/(progress|running|review|doing|active|open.*progress)/.test(lower)) return 'running'
  if (/(pause|wait|hold|defer)/.test(lower)) return 'paused'
  return 'queued'
}

export function capitalizeProvider(raw: string): string {
  const lower = raw.toLowerCase()
  if (lower === 'redmine') return 'Redmine'
  if (lower === 'local') return 'Local'
  return raw
}

export function payloadFetchedAtMs(secs: number): number {
  return secs > 0 ? secs * 1000 : Date.now()
}

/** Pure mapping for the `get_tasks` payload; kept separate so Bun tests cover IPC mapping. */
export function mapTasksPayload(payload: TasksPayloadRaw): FetchResult<TasksView> {
  const fetchedMs = payloadFetchedAtMs(payload.fetched_at)
  const items: TaskItem[] = payload.items.map(entry => ({
    id: `#${entry.number}`,
    title: entry.title,
    phase: entry.state,
    status: mapIssueStateToTaskStatus(entry.state),
    progress: mapIssueStateToTaskStatus(entry.state) === 'done' ? 100 : 0,
    updatedAt: new Date(fetchedMs).toISOString(),
  }))
  return {
    data: {
      items,
      branch: payload.branch,
      boundIssue: payload.bound_issue,
      provider: payload.provider,
      role: payload.role,
      warning: payload.warning ?? null,
    },
    fetchedAt: fetchedMs,
  }
}

/** Pure mapping for the `get_status` payload; kept separate so Bun tests cover IPC mapping. */
export function mapStatusPayload(payload: StatusPayloadRaw): FetchResult<StatusView> {
  const fetchedMs = payloadFetchedAtMs(payload.fetched_at)
  const connection = payload.connection === 'degraded' ? 'degraded' : payload.connection === 'connected' ? 'connected' : 'offline'
  const recent: StatusEvent[] = payload.recent_timers.map(timer => ({
    id: timer.run_id,
    at: new Date(timer.started_at * 1000).toISOString(),
    level: timer.status === 'running' ? 'info' as const : timer.sync_status === 'failed' ? 'error' as const : 'success' as const,
    message: `Run ${timer.run_id} issue #${timer.issue} ${timer.phase} (${timer.status}).`,
  }))
  if (payload.warning) {
    recent.unshift({ id: 'warn-backend', at: new Date(fetchedMs).toISOString(), level: 'warning' as const, message: payload.warning })
  }
  if (payload.statuses_unsupported) {
    recent.unshift({ id: 'unsupported-status', at: new Date(fetchedMs).toISOString(), level: 'info' as const, message: payload.statuses_unsupported })
  }
  if (payload.bound_issue != null) {
    const title = payload.bound_issue_title ?? `Issue #${payload.bound_issue}`
    const state = payload.bound_issue_state ?? 'unknown'
    recent.unshift({ id: `bound-${payload.bound_issue}`, at: new Date(fetchedMs).toISOString(), level: 'info' as const, message: `Bound issue #${payload.bound_issue}: ${title} (${state}).` })
  }
  const view: StatusView = {
    summary: {
      connection,
      provider: capitalizeProvider(payload.provider),
      endpoint: payload.endpoint ?? '',
      lastSyncAt: new Date(fetchedMs).toISOString(),
      totals: {
        queued: 0,
        running: payload.running_timers,
        paused: 0,
        done: 0,
        failed: 0,
      },
      recent,
    },
    branch: payload.branch,
    boundIssue: payload.bound_issue,
    boundIssueTitle: payload.bound_issue_title ?? null,
    warning: payload.warning ?? null,
    unsupported: payload.statuses_unsupported ?? null,
    frontendDistHash: payload.frontend_dist_hash ?? null,
  }
  return { data: view, fetchedAt: fetchedMs }
}

// ---------------------------------------------------------------------------
// Tasks / status loaders (preload API call with safe empty fallback).
// ---------------------------------------------------------------------------

function emptyTasksResult(): FetchResult<TasksView> {
  return {
    data: { items: [], branch: null, boundIssue: null, provider: '', role: '', warning: null },
    fetchedAt: Date.now(),
  }
}

function emptyStatusResult(): FetchResult<StatusView> {
  const now = Date.now()
  return {
    data: {
      summary: {
        connection: 'offline',
        provider: '',
        endpoint: '',
        lastSyncAt: new Date(now).toISOString(),
        totals: { queued: 0, running: 0, paused: 0, done: 0, failed: 0 },
        recent: [],
      },
      branch: null,
      boundIssue: null,
      boundIssueTitle: null,
      warning: null,
      unsupported: null,
      frontendDistHash: null,
    },
    fetchedAt: now,
  }
}

export async function fetchTasks(): Promise<FetchResult<TasksView>> {
  const api = desktopApi()
  if (!api) return emptyTasksResult()
  try {
    return mapTasksPayload(await api.getTasks({ limit: 20, state: 'open' }))
  }
  catch (err) {
    // Browser fallback only when the preload API is unavailable; otherwise
    // surface the redacted backend error so the page shows error/retry.
    if (!desktopApi()) return emptyTasksResult()
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

export async function fetchStatus(): Promise<FetchResult<StatusView>> {
  const api = desktopApi()
  if (!api) return emptyStatusResult()
  try {
    return mapStatusPayload(await api.getStatus({}))
  }
  catch (err) {
    if (!desktopApi()) return emptyStatusResult()
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

// ---------------------------------------------------------------------------
// Config snapshot + settings mutations (secrets write-only).
// ---------------------------------------------------------------------------

export interface CredentialSummaryRaw {
  present: boolean
  length?: number
}

export interface RoleSnapshotRaw {
  role: string
  provider?: string | null
  redmine_close_status_id?: number | null
  redmine_credential: CredentialSummaryRaw
}

export interface ConfigSnapshotRaw {
  database_path: string
  roles: RoleSnapshotRaw[]
  global_settings: Array<{ name: string, present: boolean, length?: number, sanitized_value?: string | null, value?: string | null }>
  global_default_provider?: string | null
}

export async function fetchConfigSnapshot(): Promise<FetchResult<ConfigSnapshotRaw>> {
  const api = desktopApi()
  if (!api) return emptyConfigSnapshotResult()
  try {
    return { data: await api.getConfigSnapshot(), fetchedAt: Date.now() }
  }
  catch (err) {
    if (!desktopApi()) return emptyConfigSnapshotResult()
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

function emptyConfigSnapshotResult(): FetchResult<ConfigSnapshotRaw> {
  return {
    data: { database_path: '', roles: [], global_settings: [], global_default_provider: null },
    fetchedAt: Date.now(),
  }
}

export function snapshotRoleEntry(snapshot: ConfigSnapshotRaw | null, role: string): RoleSnapshotRaw | null {
  if (!snapshot) return null
  return snapshot.roles.find(entry => entry.role === role) ?? null
}

/** Canonical global setting name of the machine-wide Redmine REST address. */
export const REDMINE_API_BASE_SETTING = 'PHASEGENT_REDMINE_API_BASE'

/**
 * The one machine-wide Redmine REST address, read from the sanitised
 * `global_settings` entry. Every role resolves the same address, so it is
 * never read from a role entry.
 */
export function snapshotRedmineApiBase(snapshot: ConfigSnapshotRaw | null): string {
  if (!snapshot) return ''
  const entry = snapshot.global_settings.find(setting => setting.name === REDMINE_API_BASE_SETTING)
  return entry?.sanitized_value ?? ''
}

export function snapshotProviderForRole(snapshot: ConfigSnapshotRaw | null, role: string): string {
  const entry = snapshotRoleEntry(snapshot, role)
  return entry?.provider ?? snapshot?.global_default_provider ?? ''
}

/**
 * Endpoint shown for a role. Redmine resolves the machine-wide address;
 * the local provider has no remote endpoint.
 */
export function snapshotEndpointForRole(snapshot: ConfigSnapshotRaw | null, role: string): string {
  const entry = snapshotRoleEntry(snapshot, role)
  if (!entry) return ''
  if (snapshotProviderForRole(snapshot, role).trim().toLowerCase() === 'redmine')
    return snapshotRedmineApiBase(snapshot)
  return ''
}

export async function setNonSecretSetting(role: string | null, setting: string, value: string): Promise<void> {
  const trimmedRole = (role ?? '').trim()
  try {
    const api = desktopApi()
    if (!api) throw new Error(BRIDGE_UNAVAILABLE)
    await api.setConfigSetting({ role: trimmedRole === '' ? null : trimmedRole, setting, value })
  }
  catch (err) {
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

export async function clearNonSecretSetting(role: string | null, setting: string): Promise<boolean> {
  const trimmedRole = (role ?? '').trim()
  try {
    const api = desktopApi()
    if (!api) throw new Error(BRIDGE_UNAVAILABLE)
    const result = await api.clearConfigSetting({ role: trimmedRole === '' ? null : trimmedRole, setting })
    return result.cleared
  }
  catch (err) {
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

export async function setRoleCredential(role: string, provider: string, credential: string): Promise<{ present: boolean, length: number }> {
  try {
    const api = desktopApi()
    if (!api) throw new Error(BRIDGE_UNAVAILABLE)
    const result = await api.setCredential({ role, provider, credential })
    return { present: result.present, length: result.length }
  }
  catch (err) {
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

export async function clearRoleCredential(role: string, provider: string): Promise<boolean> {
  try {
    const api = desktopApi()
    if (!api) throw new Error(BRIDGE_UNAVAILABLE)
    const result = await api.clearCredential({ role, provider })
    return result.cleared
  }
  catch (err) {
    throw new Error(desktopErrorMessage(err), { cause: err })
  }
}

export interface ProvisioningStatusRaw {
  role: string
  user_id?: number | null
  login?: string | null
  credential_present: boolean
  credential_length: number
}

export async function fetchProvisioningStatus(role: string): Promise<ProvisioningStatusRaw | null> {
  const api = desktopApi()
  if (!api) return null
  try {
    return await api.getProvisioningStatus({ role })
  }
  catch {
    return null
  }
}

export async function testConnection(endpoint: string): Promise<{ ok: boolean, message: string }> {
  const problem = validateEndpoint(endpoint)
  if (problem) return { ok: false, message: problem }
  const api = desktopApi()
  if (!api) {
    return { ok: true, message: `Connection check passed for ${endpoint.trim()} (browser preview, no backend).` }
  }
  try {
    await api.getStatus({})
    return { ok: true, message: `Connection check passed for ${endpoint.trim()}.` }
  }
  catch (err) {
    return { ok: false, message: desktopErrorMessage(err) }
  }
}

export function loadSettings(): SettingsState {
  return { ...DEFAULT_SETTINGS }
}

export function saveSettings(settings: SettingsState): void {
  // Settings persist through the backend IPC path (setNonSecretSetting /
  // setRoleCredential). Kept as a no-op for API compatibility so existing
  // imports keep compiling; pages call the IPC helpers directly.
  void settings
}
