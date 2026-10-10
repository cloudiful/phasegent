//! Role and kind denial gates: every denial is a structured `permission`
//! envelope on stderr with exit 3 and nothing on stdout, and it fires
//! before the provider is resolved.

use super::fixtures::{Scratch, error_envelope, run_local};
use super::support::{stderr_text, stdout_text};

/// Every denial is a structured `permission` envelope on stderr with exit
/// 3 and nothing on stdout, and it fires before the provider is resolved.
#[test]
fn record_create_denials_fire_before_any_provider_access() {
    let scratch = Scratch::new();
    let db = scratch.local_db();

    let deny = |role: &str, args: &[&str]| -> serde_json::Value {
        let out = run_local(&db, role, args);
        assert_eq!(
            out.status.code(),
            Some(3),
            "role={role} args={args:?} stderr={}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "", "a denial prints nothing on stdout");
        error_envelope(&out)
    };

    // admin is never an agent role.
    let envelope = deny(
        "admin",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "record create");

    // executor may not publish another role's kind.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "reviewer",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--review",
            "final",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reviewer")
    );

    // recon belongs to explore, not executor.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "recon",
            "--key",
            "k",
            "--recon",
            "scan",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("recon")
    );

    // explore may only publish recon...
    let envelope = deny(
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
            "--authorized",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("executor")
    );

    // ...and only with explicit authorization.
    let envelope = deny(
        "explore",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "recon",
            "--key",
            "k",
            "--recon",
            "scan",
            "--body",
            "n",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--authorized")
    );

    // A child role always needs the explicit flag, even for its own kind.
    let envelope = deny(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "create",
            "1",
            "--kind",
            "executor",
            "--key",
            "k",
            "--phase",
            "P1",
            "--attempt",
            "1",
            "--body",
            "n",
        ],
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--authorized")
    );
}
