//! Complete-report emission and exit-code derivation for the global bootstrap.

use crate::workflow::GlobalBootstrapReport;

/// Serialize the complete report and pair it with the process exit code so the
/// printed payload and the outcome can never disagree. `0` when every project
/// is ready, `1` when any project is blocked, warned, or errored.
pub(super) fn payload(report: &GlobalBootstrapReport) -> (serde_json::Value, i32) {
    let value = serde_json::to_value(report).unwrap_or_else(
        |error| serde_json::json!({"kind": "encode", "message": error.to_string()}),
    );
    (value, report.exit_code())
}

/// Print the complete report on stdout and return its exit code.
pub(super) fn emit(report: &GlobalBootstrapReport) -> i32 {
    let (value, code) = payload(report);
    let printed = crate::cli::print_json(&value);
    if printed != 0 { printed } else { code }
}

#[cfg(test)]
mod tests {
    use super::payload;
    use crate::workflow::{
        GlobalBootstrapReport, MembershipReport, ProjectReport, RoleIdentityReport,
    };

    fn role(status: &str) -> RoleIdentityReport {
        RoleIdentityReport {
            role: "orchestrator".to_owned(),
            login: "phasegent-orchestrator".to_owned(),
            user_id: Some(11),
            role_name: "Maintainer".to_owned(),
            status: status.to_owned(),
        }
    }

    fn membership(status: &str) -> MembershipReport {
        MembershipReport {
            role: "orchestrator".to_owned(),
            user_id: Some(11),
            login: "phasegent-orchestrator".to_owned(),
            role_name: "Maintainer".to_owned(),
            status: status.to_owned(),
            warning: None,
        }
    }

    #[test]
    fn ready_report_serializes_the_global_contract_and_exits_zero() {
        let report = GlobalBootstrapReport::new(
            true,
            vec![role("existing")],
            vec![ProjectReport::new(
                44,
                "owner-repo".to_owned(),
                vec![membership("planned")],
                None,
            )],
        );
        let (value, code) = payload(&report);
        assert_eq!(code, 0);
        assert_eq!(value["mode"], "all-projects");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["ready"], true);
        assert_eq!(value["roles"][0]["login"], "phasegent-orchestrator");
        assert_eq!(value["projects"][0]["project_id"], 44);
        assert_eq!(value["projects"][0]["memberships"][0]["status"], "planned");
        assert_eq!(value["summary"]["projects"], 1);
        assert_eq!(value["summary"]["planned"], 1);
        assert_eq!(value["summary"]["errors"], 0);
    }

    #[test]
    fn blocked_or_warned_report_serializes_nonzero() {
        let report = GlobalBootstrapReport::new(
            false,
            vec![role("provisioned")],
            vec![ProjectReport::new(
                44,
                "owner-repo".to_owned(),
                vec![membership("warning")],
                None,
            )],
        );
        let (value, code) = payload(&report);
        assert_eq!(code, 1);
        assert_eq!(value["ready"], false);
        assert_eq!(value["summary"]["warnings"], 1);
    }
}
