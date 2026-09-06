use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{RedmineNewUser, RedmineUser};

impl RedmineProvider {
    /// Create a user with the admin REST API (`POST /users.json`).
    ///
    /// The provider must have been constructed with an administrator API
    /// key. Identity fields are validated locally so empty logins or
    /// mails fail fast with a `config` error before any HTTP request;
    /// Redmine-side validation (duplicate login, malformed mail) arrives
    /// as an `Http` 422 through the shared redacted error path. The
    /// creation response rarely carries the new user's API key; call
    /// [`Self::get_user_api_key`] after creation to retrieve it.
    ///
    /// Phase 1 introduces this helper for contract tests; phase 2 wires
    /// it into bootstrap provisioning.
    #[allow(dead_code)]
    pub fn create_user(
        &self,
        login: &str,
        firstname: &str,
        lastname: &str,
        mail: &str,
        password: &str,
    ) -> Result<RedmineUser, ForgejoError> {
        if login.trim().is_empty() {
            return Err(ForgejoError::config("Redmine user login cannot be empty"));
        }
        if firstname.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine user firstname cannot be empty",
            ));
        }
        if lastname.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine user lastname cannot be empty",
            ));
        }
        if mail.trim().is_empty() {
            return Err(ForgejoError::config("Redmine user mail cannot be empty"));
        }
        if password.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine user password cannot be empty",
            ));
        }
        let payload = RedmineNewUser::new(login, firstname, lastname, mail, Some(password));
        self.http.create_user(&payload)
    }

    /// Read a user by id with the admin REST API
    /// (`GET /users/:id.json`). When called with an administrator key
    /// the response exposes that user's `api_key` on
    /// [`RedmineUser::api_key`].
    ///
    /// Phase 1 introduces this helper for contract tests; phase 2 wires
    /// it into bootstrap provisioning.
    #[allow(dead_code)]
    pub fn get_user(&self, id: u64) -> Result<RedmineUser, ForgejoError> {
        if id == 0 {
            return Err(ForgejoError::config(
                "Redmine user id must be greater than zero",
            ));
        }
        self.http.get_user(id)
    }

    /// Read a user's API key by id. Wraps [`Self::get_user`] and extracts
    /// the admin-exposed `api_key`; a response without a usable key
    /// surfaces as a `decode` error that never echoes the raw payload so
    /// no key material can leak into the message.
    ///
    /// Phase 1 introduces this helper for contract tests; phase 2 wires
    /// it into bootstrap provisioning.
    #[allow(dead_code)]
    pub fn get_user_api_key(&self, id: u64) -> Result<String, ForgejoError> {
        let user = self.get_user(id)?;
        user.api_key_value()
            .filter(|value| !value.chars().any(char::is_control))
            .map(str::to_owned)
            .ok_or_else(|| ForgejoError::Decode {
                operation: "user get".to_owned(),
                message: "Redmine user response missing API key".to_owned(),
            })
    }
}
