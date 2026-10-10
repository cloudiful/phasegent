//! Redmine user REST contract tests.
//!
//! Split by responsibility: [`crud`] for create/read/validation/redaction,
//! [`lookup`] for the paginated login scan used by provisioning, and
//! [`provisioning`] for the deterministic service-account contract.

mod crud;
mod lookup;
mod provisioning;

pub(crate) const USER_API_SECRET: &str = "user-secret-key-9f8e7d6c5b4a";
pub(crate) const DECODE_SECRET: &str = "user-secret-decode-abcdef123456";

pub(crate) fn user_response(id: u64, login: &str, api_key: Option<&str>) -> String {
    let mut user = serde_json::json!({
        "id": id,
        "login": login,
        "firstname": "Test",
        "lastname": "User",
        "mail": format!("{login}@example.test"),
        "created_on": "2026-01-01T00:00:00Z",
        "status": 1,
    });
    if let Some(key) = api_key {
        user["api_key"] = serde_json::Value::String(key.to_owned());
    }
    serde_json::json!({ "user": user }).to_string()
}

pub(crate) fn user_list_response(users: &[(u64, &str)]) -> String {
    serde_json::json!({
        "users": users.iter().map(|(id, login)| serde_json::json!({
            "id": id,
            "login": login,
            "firstname": "Phasegent",
            "lastname": login,
            "mail": format!("{login}@phasegent.local"),
        })).collect::<Vec<_>>(),
        "total_count": users.len(),
        "limit": 100,
    })
    .to_string()
}
