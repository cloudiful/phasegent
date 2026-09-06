use serde::{Deserialize, Serialize};

/// Redmine user record returned by the admin REST API (`POST /users.json`
/// and `GET /users/:id.json`).
///
/// `api_key` is present only on admin reads (and on the requesting user's
/// own record); creation responses may omit it, so it is `Option`. The
/// custom `Debug` impl redacts the key so panic messages and debug logs
/// never expose it.
///
/// Phase 1 introduces this model for contract tests; phase 2 wires it
/// into bootstrap provisioning.
#[derive(Clone, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct RedmineUser {
    pub id: u64,
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub firstname: String,
    #[serde(default)]
    pub lastname: String,
    #[serde(default)]
    pub mail: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub status: Option<u64>,
    #[serde(default)]
    pub admin: Option<bool>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub last_login_on: Option<String>,
}

impl std::fmt::Debug for RedmineUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedmineUser")
            .field("id", &self.id)
            .field("login", &self.login)
            .field("firstname", &self.firstname)
            .field("lastname", &self.lastname)
            .field("mail", &self.mail)
            .field("api_key", &self.api_key.as_deref().map(|_| "[redacted]"))
            .field("status", &self.status)
            .field("admin", &self.admin)
            .field("created_on", &self.created_on)
            .field("last_login_on", &self.last_login_on)
            .finish()
    }
}

impl RedmineUser {
    /// Borrow the API key when the admin read exposed one. Returns `None`
    /// for creation responses or non-admin reads that omit the field, and
    /// for blank values that carry no credential.
    #[allow(dead_code)]
    pub fn api_key_value(&self) -> Option<&str> {
        self.api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RedmineUserResponse {
    pub(crate) user: RedmineUser,
}

/// Admin user-creation payload for `POST /users.json`.
///
/// Field names mirror the Redmine REST API (`login`, `firstname`,
/// `lastname`, `mail`, `password`, plus the optional provisioning flags).
/// Optional fields are omitted from JSON when unset so the minimal create
/// request stays byte-stable. The custom `Debug` impl redacts `password`
/// so the credential never appears in panic messages or debug logs.
#[derive(Serialize)]
#[allow(dead_code)]
pub(crate) struct RedmineNewUser<'a> {
    pub(crate) user: RedmineNewUserFields<'a>,
}

#[derive(Serialize)]
#[allow(dead_code)]
pub(crate) struct RedmineNewUserFields<'a> {
    pub(crate) login: &'a str,
    pub(crate) firstname: &'a str,
    pub(crate) lastname: &'a str,
    pub(crate) mail: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) password: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) generate_password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) must_change_passwd: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) admin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<u64>,
}

#[allow(dead_code)]
impl<'a> RedmineNewUser<'a> {
    pub(crate) fn new(
        login: &'a str,
        firstname: &'a str,
        lastname: &'a str,
        mail: &'a str,
        password: Option<&'a str>,
    ) -> Self {
        Self {
            user: RedmineNewUserFields {
                login,
                firstname,
                lastname,
                mail,
                password,
                generate_password: None,
                must_change_passwd: None,
                admin: None,
                status: None,
            },
        }
    }

    pub(crate) fn with_generate_password(mut self, value: bool) -> Self {
        self.user.generate_password = Some(value);
        self
    }

    pub(crate) fn with_must_change_passwd(mut self, value: bool) -> Self {
        self.user.must_change_passwd = Some(value);
        self
    }

    pub(crate) fn with_admin(mut self, value: bool) -> Self {
        self.user.admin = Some(value);
        self
    }

    pub(crate) fn with_status(mut self, value: u64) -> Self {
        self.user.status = Some(value);
        self
    }
}

impl<'a> std::fmt::Debug for RedmineNewUser<'a> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedmineNewUser")
            .field("login", &self.user.login)
            .field("firstname", &self.user.firstname)
            .field("lastname", &self.user.lastname)
            .field("mail", &self.user.mail)
            .field("password", &self.user.password.map(|_| "[redacted]"))
            .field("generate_password", &self.user.generate_password)
            .field("must_change_passwd", &self.user.must_change_passwd)
            .field("admin", &self.user.admin)
            .field("status", &self.user.status)
            .finish()
    }
}
