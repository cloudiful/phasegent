// Typed preload API builder.
//
// Every method maps onto one allowlisted channel through the shared method
// table; the renderer receives only these ten functions and never an IPC
// channel name, `ipcRenderer`, a CLI command, or a credential.

import type {
  AppMetadata,
  BranchContext,
  ClearCredentialRequest,
  ClearCredentialResponse,
  ClearSettingRequest,
  ClearSettingResponse,
  ConfigSnapshot,
  CredentialPresence,
  DesktopMethod,
  ProvisioningQuery,
  ProvisioningStatus,
  SetCredentialRequest,
  SetSettingRequest,
  SetSettingResponse,
  StatusPayload,
  StatusRequest,
  TasksPayload,
  TasksRequest,
} from '../shared/methods'

/** Transport callback supplied by the preload script. */
export type DesktopInvoke = (method: DesktopMethod, payload?: unknown) => Promise<unknown>

export interface PhasegentDesktopApi {
  getAppMetadata(): Promise<AppMetadata>
  getConfigSnapshot(): Promise<ConfigSnapshot>
  getBranchContext(): Promise<BranchContext>
  getTasks(request?: TasksRequest): Promise<TasksPayload>
  getStatus(request?: StatusRequest): Promise<StatusPayload>
  setConfigSetting(request: SetSettingRequest): Promise<SetSettingResponse>
  clearConfigSetting(request: ClearSettingRequest): Promise<ClearSettingResponse>
  setCredential(request: SetCredentialRequest): Promise<CredentialPresence>
  clearCredential(request: ClearCredentialRequest): Promise<ClearCredentialResponse>
  getProvisioningStatus(query: ProvisioningQuery): Promise<ProvisioningStatus>
}

/**
 * Build the API exposed on `window.phasegent`. The method set is fixed at
 * compile time: there is no dynamic method name, channel, or argument vector.
 */
export function createDesktopApi(invoke: DesktopInvoke): PhasegentDesktopApi {
  return {
    getAppMetadata: () => invoke('get_app_metadata') as Promise<AppMetadata>,
    getConfigSnapshot: () => invoke('get_config_snapshot') as Promise<ConfigSnapshot>,
    getBranchContext: () => invoke('get_branch_context') as Promise<BranchContext>,
    getTasks: (request: TasksRequest = {}) => invoke('get_tasks', request) as Promise<TasksPayload>,
    getStatus: (request: StatusRequest = {}) => invoke('get_status', request) as Promise<StatusPayload>,
    setConfigSetting: (request: SetSettingRequest) =>
      invoke('set_config_setting', request) as Promise<SetSettingResponse>,
    clearConfigSetting: (request: ClearSettingRequest) =>
      invoke('clear_config_setting', request) as Promise<ClearSettingResponse>,
    setCredential: (request: SetCredentialRequest) =>
      invoke('set_credential', request) as Promise<CredentialPresence>,
    clearCredential: (request: ClearCredentialRequest) =>
      invoke('clear_credential', request) as Promise<ClearCredentialResponse>,
    getProvisioningStatus: (query: ProvisioningQuery) =>
      invoke('get_provisioning_status', query) as Promise<ProvisioningStatus>,
  }
}
