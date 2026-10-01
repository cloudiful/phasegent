// Desktop bridge contract shared by the Electron main process and the preload
// script. The wire names, parameter fields, and payload shapes mirror
// `src/desktop_bridge.rs` (protocol v1) and `src/gui/models.rs`; the contract
// test in `methods.test.ts` reads that Rust source so the two cannot drift.
//
// Imported by both `electron/main.ts` and `electron/preload.ts`, which are
// single-entry bundles, so the table is inlined into each bundle instead of
// being shared through a chunk the sandboxed preload cannot require.

/** Wire protocol version carried by every bridge response. */
export const DESKTOP_PROTOCOL_VERSION = 1

/**
 * Largest renderer payload accepted by the main process and the bridge client.
 */
export const MAX_PAYLOAD_BYTES = 47 * 1024

/**
 * Largest serialized request line the client writes. Stays below the Rust
 * bridge's 64 KiB line limit so no accepted request is rejected as
 * `too_large`, and leaves room for the `id`/`method`/`params` envelope.
 */
export const MAX_REQUEST_BYTES = 48 * 1024

/** Application identity reported by `get_app_metadata`. */
export interface AppMetadata {
  name: string
  version: string
  identifier: string
}

/** Current branch binding; `warning` carries a bounded Git failure reason. */
export interface BranchContext {
  branch: string | null
  issue_id: number | null
  warning?: string | null
}

/** Bounded task-list request; every field is optional. */
export interface TasksRequest {
  role?: string | null
  provider?: string | null
  limit?: number | null
  state?: string | null
}

/** Single task row; `url` is sanitised (no userinfo/query/fragment). */
export interface TaskEntry {
  number: number
  title: string
  state: string
  url?: string | null
}

export interface TasksPayload {
  branch: string | null
  bound_issue: number | null
  provider: string
  role: string
  items: TaskEntry[]
  total_count?: number | null
  has_more: boolean
  data_source: string
  fetched_at: number
  warning?: string | null
}

export interface StatusRequest {
  role?: string | null
  provider?: string | null
}

/** Minimal timer row; no secrets and no projection tokens. */
export interface TimerDto {
  run_id: string
  issue: number
  phase: string
  role: string
  status: string
  sync_status: string
  started_at: number
  finished_at?: number | null
}

export interface StatusPayload {
  branch: string | null
  bound_issue: number | null
  bound_issue_title?: string | null
  bound_issue_state?: string | null
  provider: string
  role: string
  endpoint?: string | null
  connection: string
  running_timers: number
  recent_timers: TimerDto[]
  fetched_at: number
  warning?: string | null
  statuses_unsupported?: string | null
  frontend_dist_hash?: string | null
}

/** Non-secret setting mutation; credential settings are rejected by the backend. */
export interface SetSettingRequest {
  role?: string | null
  setting: string
  value: string
}

export interface SetSettingResponse {
  setting: string
  role?: string | null
  updated: boolean
}

export interface ClearSettingRequest {
  role?: string | null
  setting: string
}

export interface ClearSettingResponse {
  setting: string
  role?: string | null
  cleared: boolean
}

/** Write-only credential set; the response never echoes the value. */
export interface SetCredentialRequest {
  role: string
  provider: string
  credential: string
}

export interface CredentialPresence {
  role: string
  provider: string
  present: boolean
  length: number
  source: string
}

export interface ClearCredentialRequest {
  role: string
  provider: string
}

export interface ClearCredentialResponse {
  role: string
  provider: string
  cleared: boolean
}

export interface ProvisioningQuery {
  role: string
}

export interface ProvisioningStatus {
  role: string
  user_id?: number | null
  login?: string | null
  credential_present: boolean
  credential_length: number
}

export interface CredentialSummary {
  present: boolean
  length?: number
  fingerprint?: string | null
  updated_at?: number | null
}

export interface RoleSnapshot {
  role: string
  provider?: string | null
  forgejo_api_base?: string | null
  forgejo_repository?: string | null
  redmine_close_status_id?: number | null
  gitlab_api_base?: string | null
  forgejo_credential: CredentialSummary
  redmine_credential: CredentialSummary
  gitlab_credential: CredentialSummary
}

export interface GlobalSettingSnapshot {
  name: string
  present: boolean
  length?: number
  sanitized_value?: string | null
  value?: string | null
}

/** Redacted `config show` snapshot; credentials report presence/length only. */
export interface ConfigSnapshot {
  database_path: string
  roles: RoleSnapshot[]
  global_settings: GlobalSettingSnapshot[]
  global_default_provider?: string | null
}

/** The versioned, allowlisted desktop method set. */
export type DesktopMethod =
  | 'get_app_metadata'
  | 'get_config_snapshot'
  | 'get_branch_context'
  | 'get_tasks'
  | 'get_status'
  | 'set_config_setting'
  | 'clear_config_setting'
  | 'set_credential'
  | 'clear_credential'
  | 'get_provisioning_status'

export interface DesktopMethodSpec {
  /** IPC channel handled by the main process. */
  readonly channel: string
  /**
   * Parameter object field the Rust bridge decodes for this method, or `null`
   * when the method takes no parameters.
   */
  readonly param: 'request' | 'query' | null
}

/** Channel prefix keeps desktop IPC namespaces separate from any other channel. */
const CHANNEL_PREFIX = 'phasegent'

function channel(method: DesktopMethod): string {
  return `${CHANNEL_PREFIX}:${method}`
}

export const DESKTOP_METHODS: Readonly<Record<DesktopMethod, DesktopMethodSpec>> = {
  get_app_metadata: { channel: channel('get_app_metadata'), param: null },
  get_config_snapshot: { channel: channel('get_config_snapshot'), param: null },
  get_branch_context: { channel: channel('get_branch_context'), param: null },
  get_tasks: { channel: channel('get_tasks'), param: 'request' },
  get_status: { channel: channel('get_status'), param: 'request' },
  set_config_setting: { channel: channel('set_config_setting'), param: 'request' },
  clear_config_setting: { channel: channel('clear_config_setting'), param: 'request' },
  set_credential: { channel: channel('set_credential'), param: 'request' },
  clear_credential: { channel: channel('clear_credential'), param: 'request' },
  get_provisioning_status: { channel: channel('get_provisioning_status'), param: 'query' },
}

/** Every desktop method in protocol order. */
export const DESKTOP_METHOD_NAMES = Object.keys(DESKTOP_METHODS) as DesktopMethod[]

/** Every channel the main process may handle. */
export const DESKTOP_CHANNELS: readonly string[] = DESKTOP_METHOD_NAMES.map(
  method => DESKTOP_METHODS[method].channel,
)

/** True when `name` is one of the allowlisted desktop methods. */
export function isDesktopMethod(name: string): name is DesktopMethod {
  return Object.prototype.hasOwnProperty.call(DESKTOP_METHODS, name)
}

/**
 * Resolve the channel for an allowlisted method. Throws for anything else so a
 * caller can never reach an unlisted channel even by casting.
 */
export function channelFor(method: DesktopMethod): string {
  const entry = DESKTOP_METHODS[method]
  if (!entry) {
    throw new Error(`unsupported desktop method '${String(method)}'`)
  }
  return entry.channel
}
