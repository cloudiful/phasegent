use super::*;

#[test]
fn redmine_stored_config_round_trips_group_selection_and_legacy_defaults() {
    // Backward-compatible decode: old configs that still carry
    // `group_name`/`group_role` from the legacy `AI Agents` workflow keep
    // deserializing without error so operators do not lose their saved
    // credentials when upgrading.
    let legacy = serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
        "group_name": "AI Agents",
        "group_role": "开发人员",
    });
    let decoded: auth::RedmineStoredConfig =
        serde_json::from_value(legacy).expect("legacy config must decode");
    assert_eq!(decoded.group_name.as_deref(), Some("AI Agents"));
    assert_eq!(decoded.group_role.as_deref(), Some("开发人员"));

    // Fresh configs no longer carry the legacy group fields but still
    // round-trip through the persistence path.
    let minimal: auth::RedmineStoredConfig = serde_json::from_value(serde_json::json!({
        "api_base": "https://redmine.example",
        "project_id": "44",
        "close_status_id": 5,
    }))
    .unwrap();
    assert_eq!(minimal.group_name, None);
    assert_eq!(minimal.group_role, None);
}

#[test]
fn provider_kind_gitlab_round_trips_and_rejects_unknown_values() {
    // Phase-1 GitLab foundation: the FromStr surface must recognise
    // "gitlab", the as_str helper must return "gitlab", and a non-
    // canonical value must surface a structured error that lists all
    // three supported providers. The docstring on the parse error is
    // what operators see in `phasegent --provider typo` so the
    // message must stay accurate.
    use std::str::FromStr;

    let parsed: ProviderKind = "gitlab".parse().expect("gitlab must parse");
    assert_eq!(parsed, ProviderKind::Gitlab);
    assert_eq!(parsed.as_str(), "gitlab");

    // Inverse direction: as_str feeds back into parse without a round
    // trip misclassification (e.g. forgetting a lowercase match arm).
    let round_trip = ProviderKind::from_str(ProviderKind::Gitlab.as_str())
        .expect("as_str must parse back to Gitlab");
    assert_eq!(round_trip, ProviderKind::Gitlab);

    // Forgejo and Redmine must continue to parse so the existing CLI
    // `--provider forgejo|redmine` paths still work.
    assert_eq!(
        "forgejo".parse::<ProviderKind>().unwrap(),
        ProviderKind::Forgejo
    );
    assert_eq!(
        "redmine".parse::<ProviderKind>().unwrap(),
        ProviderKind::Redmine
    );

    let error = "wrong".parse::<ProviderKind>().unwrap_err();
    assert!(
        error.contains("forgejo, redmine, gitlab, or local"),
        "parse error must enumerate the supported providers: {error}"
    );
}

#[test]
fn provider_kind_local_round_trips_and_resolves_without_credentials() {
    // `local` must parse/render/display like the other providers,
    // round-trip through `ProviderKind::from_str`, and resolve through
    // the persisted-default chain so `--provider local` commands need no
    // credential and no network.
    use std::str::FromStr;

    let parsed: ProviderKind = "local".parse().expect("local must parse");
    assert_eq!(parsed, ProviderKind::Local);
    assert_eq!(parsed.as_str(), "local");
    assert_eq!(format!("{parsed}"), "local");

    let round_trip = ProviderKind::from_str(ProviderKind::Local.as_str())
        .expect("as_str must parse back to Local");
    assert_eq!(round_trip, ProviderKind::Local);

    // The top-level `--provider local` flag flows through the parser.
    let args = [
        "--role",
        "executor",
        "--provider",
        "local",
        "issue",
        "search",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("--provider local must parse");
    assert_eq!(
        invocation.provider.expect("--provider must be captured"),
        ProviderKind::Local
    );
}

#[test]
fn provider_flag_parses_gitlab_for_role_free_branch_commands() {
    // `--provider gitlab` must flow through the parser without error
    // so `auth setup`, `issue`, `comment`, etc. all accept the new
    // value. The branch-context commands (bind/unbind/status) are
    // provider-free in their resolver, but the top-level `--provider`
    // flag is still accepted by the outer parser.
    use std::str::FromStr;

    let args = [
        "--role",
        "orchestrator",
        "--provider",
        "gitlab",
        "issue",
        "search",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let invocation = command::parse(&args).expect("--provider gitlab must parse");
    assert_eq!(
        invocation.provider.expect("--provider must be captured"),
        ProviderKind::Gitlab
    );

    // Inline form `--provider=gitlab` is recognised too so scripts
    // that build argv with the `option=value` style still work.
    let inline = [
        "--role=orchestrator",
        "--provider=gitlab",
        "issue",
        "search",
        "--query",
        "phase-1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let parsed = command::parse(&inline).expect("--provider=gitlab must parse");
    assert_eq!(parsed.provider, Some(ProviderKind::Gitlab));

    // Sanity: as_str + FromStr cross-check at the call site.
    assert_eq!(ProviderKind::from_str("gitlab").unwrap().as_str(), "gitlab");
}
