//! Feature-gated Tauri command set and desktop shell entry.
//!
//! Normal CLI dispatch never reaches [`run`]: only the explicit `gui`
//! command and the no-argument desktop heuristic call it. Every async
//! command bridges its blocking backend entry point through
//! `spawn_blocking` so provider/storage work never runs on the Tauri
//! async runtime directly.

#[cfg(feature = "gui")]
mod imp {
    use crate::gui::{AppMetadata, app_metadata, read_config_snapshot};
    use crate::gui::{
        BranchContextPayload, ClearCredentialRequest, ClearCredentialResponse, ClearSettingRequest,
        ClearSettingResponse, CredentialPresence, ProvisioningQuery, ProvisioningStatus,
        SetCredentialRequest, SetSettingRequest, SetSettingResponse, StatusPayload, StatusRequest,
        TaskEntry, TasksPayload, TasksRequest, TimerDto,
    };
    use crate::gui::{
        clear_credential_blocking, clear_setting_blocking, read_branch_context,
        read_provisioning_blocking, read_status_blocking, read_tasks_blocking,
        write_credential_blocking, write_setting_blocking,
    };

    #[tauri::command]
    async fn get_config_snapshot() -> Result<crate::config_snapshot::ConfigSnapshot, String> {
        tauri::async_runtime::spawn_blocking(read_config_snapshot)
            .await
            .map_err(|error| format!("config snapshot task failed: {error}"))?
    }

    #[tauri::command]
    fn get_app_metadata() -> AppMetadata {
        app_metadata()
    }

    #[tauri::command]
    async fn get_branch_context() -> Result<BranchContextPayload, String> {
        tauri::async_runtime::spawn_blocking(read_branch_context)
            .await
            .map_err(|error| format!("branch context task failed: {error}"))?
    }

    #[tauri::command]
    async fn get_tasks(request: TasksRequest) -> Result<TasksPayload, String> {
        tauri::async_runtime::spawn_blocking(move || read_tasks_blocking(request))
            .await
            .map_err(|error| format!("tasks task failed: {error}"))?
    }

    #[tauri::command]
    async fn get_status(request: StatusRequest) -> Result<StatusPayload, String> {
        tauri::async_runtime::spawn_blocking(move || read_status_blocking(request))
            .await
            .map_err(|error| format!("status task failed: {error}"))?
    }

    #[tauri::command]
    async fn set_config_setting(request: SetSettingRequest) -> Result<SetSettingResponse, String> {
        tauri::async_runtime::spawn_blocking(move || write_setting_blocking(request))
            .await
            .map_err(|error| format!("set setting task failed: {error}"))?
    }

    #[tauri::command]
    async fn clear_config_setting(
        request: ClearSettingRequest,
    ) -> Result<ClearSettingResponse, String> {
        tauri::async_runtime::spawn_blocking(move || clear_setting_blocking(request))
            .await
            .map_err(|error| format!("clear setting task failed: {error}"))?
    }

    #[tauri::command]
    async fn set_credential(request: SetCredentialRequest) -> Result<CredentialPresence, String> {
        tauri::async_runtime::spawn_blocking(move || write_credential_blocking(request))
            .await
            .map_err(|error| format!("set credential task failed: {error}"))?
    }

    #[tauri::command]
    async fn clear_credential(
        request: ClearCredentialRequest,
    ) -> Result<ClearCredentialResponse, String> {
        tauri::async_runtime::spawn_blocking(move || clear_credential_blocking(request))
            .await
            .map_err(|error| format!("clear credential task failed: {error}"))?
    }

    #[tauri::command]
    async fn get_provisioning_status(
        query: ProvisioningQuery,
    ) -> Result<ProvisioningStatus, String> {
        tauri::async_runtime::spawn_blocking(move || read_provisioning_blocking(query))
            .await
            .map_err(|error| format!("provisioning task failed: {error}"))?
    }

    /// Open the Tauri desktop shell. Called only for the explicit
    /// `gui` command and the no-argument desktop heuristic; normal CLI
    /// dispatch never reaches here.
    pub fn run() -> i32 {
        // Keep unused IPC row types referenced so refactors cannot
        // silently drop a field the frontend depends on.
        let _ = std::mem::size_of::<TaskEntry>();
        let _ = std::mem::size_of::<TimerDto>();
        match tauri::Builder::default()
            .invoke_handler(tauri::generate_handler![
                get_config_snapshot,
                get_app_metadata,
                get_branch_context,
                get_tasks,
                get_status,
                set_config_setting,
                clear_config_setting,
                set_credential,
                clear_credential,
                get_provisioning_status
            ])
            .run(tauri::generate_context!())
        {
            Ok(()) => 0,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"error":{"kind":"gui", "message":format!("could not start desktop shell: {error}")}})
                );
                1
            }
        }
    }
}

#[cfg(not(feature = "gui"))]
mod imp {
    /// Stub used when the binary was built without the `gui` feature.
    /// Keeps the `gui` subcommand parsed and documented while making
    /// the missing desktop runtime an explicit structured error
    /// instead of silently falling back to CLI help.
    pub fn run() -> i32 {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"gui", "message":"GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell"}})
        );
        1
    }
}

pub use imp::run;
