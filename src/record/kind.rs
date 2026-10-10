//! The record kind vocabulary and the role binding that constrains it.
//!
//! A record's kind is fixed by the CLI session role; there is no actor
//! override, so an agent can never label its own work as another role's.

use crate::policy::Role;

/// The three record kinds an agent may publish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecordKind {
    Executor,
    Reviewer,
    Recon,
}

impl RecordKind {
    /// Every kind, in declaration order. Shared by the role-binding tests
    /// and by any caller that must reason about the whole vocabulary.
    #[cfg(test)]
    pub(crate) const ALL: &'static [RecordKind] = &[Self::Executor, Self::Reviewer, Self::Recon];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
            Self::Recon => "recon",
        }
    }

    /// The single role allowed to publish this kind. `orchestrator` is
    /// the exception: it publishes every kind, and is handled by
    /// [`RecordKind::allows`].
    fn owning_role(self) -> Role {
        match self {
            Self::Executor => Role::Executor,
            Self::Reviewer => Role::Reviewer,
            Self::Recon => Role::Explore,
        }
    }

    /// Whether `role` may publish this kind at all.
    pub(crate) const fn allows(self, role: Role) -> bool {
        match role {
            // The human bootstrap role is never an agent and publishes no
            // record kind.
            Role::Admin => false,
            Role::Orchestrator => true,
            Role::Explore => matches!(self, Self::Recon),
            Role::Executor => matches!(self, Self::Executor),
            Role::Reviewer => matches!(self, Self::Reviewer),
        }
    }

    /// The denial message for a role that may not publish `kind`. It names
    /// the requested kind (the flag value the caller can change) and the
    /// role that owns it.
    pub(crate) fn denied(self, role: Role) -> String {
        if role == Role::Admin {
            return "role 'admin' is not allowed to perform record create".to_owned();
        }
        format!(
            "role '{role}' may not create a {} record; {} records belong to the {} role",
            self,
            self,
            self.owning_role()
        )
    }
}

impl std::str::FromStr for RecordKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "executor" => Ok(Self::Executor),
            "reviewer" => Ok(Self::Reviewer),
            "recon" => Ok(Self::Recon),
            _ => Err(format!(
                "invalid record kind '{value}'; expected executor, reviewer, or recon"
            )),
        }
    }
}

impl std::fmt::Display for RecordKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_binding_follows_the_session_role() {
        assert!(RecordKind::Executor.allows(Role::Executor));
        assert!(RecordKind::Reviewer.allows(Role::Reviewer));
        assert!(RecordKind::Recon.allows(Role::Explore));
        // The orchestrator publishes every kind.
        for kind in RecordKind::ALL {
            assert!(kind.allows(Role::Orchestrator), "{kind}");
        }
        // No role publishes another role's kind.
        for kind in RecordKind::ALL {
            assert!(!kind.allows(Role::Admin), "{kind} must deny admin");
        }
        assert!(!RecordKind::Executor.allows(Role::Reviewer));
        assert!(!RecordKind::Reviewer.allows(Role::Executor));
        assert!(!RecordKind::Executor.allows(Role::Explore));
        assert!(!RecordKind::Reviewer.allows(Role::Explore));
        assert!(!RecordKind::Recon.allows(Role::Executor));
        assert!(!RecordKind::Recon.allows(Role::Reviewer));
    }
}
