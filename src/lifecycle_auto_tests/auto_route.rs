#[test]
fn auto_route_tool_table_matches_timer_roles() {
    use crate::lifecycle_auto::{ToolSignal, auto_route_next, status_to_agent_role};

    assert_eq!(
        auto_route_next(1, ToolSignal::IssueCreated),
        Some("In Progress")
    );
    assert_eq!(
        auto_route_next(1, ToolSignal::CommentCreated),
        Some("In Review")
    );
    assert_eq!(auto_route_next(1, ToolSignal::IssueClosed), Some("Closed"));

    for (signal, expected_role) in [
        (ToolSignal::IssueCreated, "executor"),
        (ToolSignal::CommentCreated, "reviewer"),
    ] {
        let target = auto_route_next(42, signal).expect("mapped signal must route");
        let (role, fallback) = status_to_agent_role(target);
        assert_eq!(role, expected_role, "signal {signal:?}");
        assert!(!fallback, "signal {signal:?} target must be canonical");
    }
}
