use super::support::{TestDb, make_test_db, phasegent_bin};
use std::path::Path;
use std::process::{Command, Stdio};

pub(super) const ALL_USERS: [(u64, &str); 4] = [
    (11, "phasegent-orchestrator"),
    (22, "phasegent-executor"),
    (33, "phasegent-reviewer"),
    (44, "phasegent-explore"),
];

pub(super) fn project_list(projects: &[(u64, &str, &str)]) -> String {
    serde_json::json!({
        "total_count": projects.len(),
        "limit": 100,
        "projects": projects
            .iter()
            .map(|(id, name, identifier)| serde_json::json!({
                "id": id,
                "name": name,
                "identifier": identifier,
                "description": "description",
                "is_public": false,
                "inherit_members": false,
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

pub(super) fn user_list(users: &[(u64, &str)]) -> String {
    serde_json::json!({
        "total_count": users.len(),
        "limit": 100,
        "users": users
            .iter()
            .map(|(id, login)| serde_json::json!({
                "id": id,
                "login": login,
                "firstname": "Phasegent",
                "lastname": login,
                "mail": format!("{login}@phasegent.local"),
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

pub(super) fn roles_list(roles: &[(u64, &str)]) -> String {
    serde_json::json!({
        "total_count": roles.len(),
        "limit": 100,
        "roles": roles
            .iter()
            .map(|(id, name)| serde_json::json!({"id": id, "name": name}))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

pub(super) fn membership_list(membership: Option<(u64, u64, &str, Vec<u64>)>) -> String {
    let memberships = membership
        .into_iter()
        .map(|(id, user_id, login, role_ids)| {
            serde_json::json!({
                "id": id,
                "user": {"id": user_id, "login": login},
                "roles": role_ids
                    .into_iter()
                    .map(|role_id| serde_json::json!({"id": role_id}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "total_count": memberships.len(),
        "limit": 100,
        "memberships": memberships,
    })
    .to_string()
}

pub(super) fn parse_stdout(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&super::support::stdout_text(output))
        .expect("stdout must be one JSON document")
}

pub(super) fn no_network_db() -> TestDb {
    make_test_db("http://127.0.0.1:1")
}

pub(super) fn run_cli_in(
    working_dir: &Path,
    db_path: &Path,
    api_base: &str,
    role: &str,
    args: &[&str],
) -> std::process::Output {
    run_cli_in_with_role(working_dir, db_path, api_base, Some(role), None, args)
}

/// Role/session-injectable spawn: `role` models `PHASEGENT_ROLE` (`None`
/// removes it for unmanaged Operator), `session` models
/// `PHASEGENT_SESSION_ID` (`None` removes it, `Some` sets it even when
/// blank). Always clears inherited role/session env plus the host
/// `OPENCODE_SESSION_ID` and the Redmine
/// overrides so Operator, managed-missing, and agent paths stay
/// deterministic (issue #859 P1).
pub(super) fn run_cli_in_with_role(
    working_dir: &Path,
    db_path: &Path,
    api_base: &str,
    role: Option<&str>,
    session: Option<&str>,
    args: &[&str],
) -> std::process::Output {
    let config_path = working_dir.join("phasegent-missing.toml");
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .current_dir(working_dir)
        .env("PHASEGENT_DB_PATH", db_path.as_os_str())
        .env("PHASEGENT_REDMINE_API_BASE", api_base)
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env("PHASEGENT_CONFIG_PATH", config_path)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    if let Some(session) = session {
        command.env("PHASEGENT_SESSION_ID", session);
    }
    command.output().expect("spawn phasegent binary")
}
