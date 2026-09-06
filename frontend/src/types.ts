// Shared view-model types for the desktop shell.
// Phase 3 replaces the mock loaders in mocks.ts with Tauri IPC calls;
// these interfaces are the contract the pages program against.

export type TaskStatus = 'queued' | 'running' | 'paused' | 'done' | 'failed'

export interface TaskItem {
  id: string
  title: string
  phase: string
  status: TaskStatus
  /** 0-100; meaningful for running/paused tasks. */
  progress: number
  updatedAt: string
}

export type ConnectionState = 'connected' | 'degraded' | 'offline'

export type StatusLevel = 'info' | 'success' | 'warning' | 'error'

export interface StatusEvent {
  id: string
  at: string
  level: StatusLevel
  message: string
}

export interface StatusSummary {
  connection: ConnectionState
  provider: string
  endpoint: string
  lastSyncAt: string
  totals: Record<TaskStatus, number>
  recent: StatusEvent[]
}

export type RoleId = 'admin' | 'orchestrator' | 'executor' | 'reviewer' | 'tester'

export interface SettingsState {
  role: RoleId
  provider: string
  endpoint: string
}

/** Page data lifecycle shared by the data views. */
export type LoadState = 'loading' | 'ready' | 'empty' | 'error'
