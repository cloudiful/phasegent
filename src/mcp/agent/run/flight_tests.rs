//! Tests for the in-process run registry's ordering guarantees.
//!
//! A checkpoint review found the in-memory handle published *after* the
//! launch task was spawned, so on a multi-threaded runtime the task
//! could remove an entry that did not exist yet and leave a live run
//! stuck, or remove a finished run's entry and keep it registered
//! forever. These pin the two invariants that close that window, plus
//! the escalation lever surviving a session going live.

use std::sync::Arc;

use super::super::run::flight::{CancelLever, Flight, FlightRegistry};

#[test]
fn a_registration_exists_before_any_task_runs() {
    let registry = FlightRegistry::default();
    let flight = registry.register("run-1").expect("registered");
    // The entry is findable straight away, with no task involved.
    let found = registry.get("run-1").expect("entry is present");
    assert!(Arc::ptr_eq(&found, &flight));
    assert!(registry.contains("run-1"));
    assert!(
        registry.register("run-1").is_err(),
        "a second start must not race the first"
    );
}

#[test]
fn a_stale_remover_cannot_delete_a_newer_registration() {
    let registry = FlightRegistry::default();
    let stale = registry.register("run-1").expect("first registration");
    registry.forget("run-1", &stale);
    assert!(!registry.contains("run-1"));

    // A task that finished late still holds the old handle. Removing
    // through it must not touch the entry a later run registered under
    // the same id, which would leave that run unkillable.
    let current = registry.register("run-1").expect("second registration");
    registry.forget("run-1", &stale);
    assert!(
        registry.contains("run-1"),
        "a stale remover must not delete the live registration"
    );
    registry.forget("run-1", &current);
    assert!(!registry.contains("run-1"));
}

#[test]
fn cancel_intent_is_recorded_even_before_a_lever_exists() {
    let registry = FlightRegistry::default();
    let flight = registry.register("run-1").expect("registered");
    // Nothing is live yet, but the intent must survive so the launch
    // task can observe it at its next stage check.
    assert!(matches!(
        registry.request_cancel("run-1"),
        CancelLever::Launching
    ));
    assert!(flight.is_cancelled());
    assert!(registry.is_cancelled("run-1"));
}

#[test]
fn a_late_abort_handle_does_not_replace_a_finished_run() {
    let registry = FlightRegistry::default();
    let flight = registry.register("run-1").expect("registered");
    let handle = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .spawn(async {})
        .abort_handle();

    // A run that finished before the handle was published must not be
    // turned back into an abortable state.
    flight.mark_done();
    flight.attach_abort(handle);
    assert!(matches!(
        registry.request_cancel("run-1"),
        CancelLever::Absent
    ));
    assert!(flight.live_session().is_none());
    assert!(!registry.abort("run-1"), "a finished run cannot be aborted");
}

#[test]
fn an_unknown_run_has_no_cancellation_lever() {
    let registry = FlightRegistry::default();
    assert!(matches!(
        registry.request_cancel("absent"),
        CancelLever::Absent
    ));
    assert!(!registry.is_cancelled("absent"));
    assert!(registry.get("absent").is_none());
    assert!(!registry.abort("absent"));
    // Forgetting an unknown id is a no-op rather than a panic.
    let orphan = Arc::new(Flight::default());
    registry.forget("absent", &orphan);
}

#[tokio::test]
async fn an_abort_handle_survives_the_session_going_live() {
    // The escalation lever is the abort handle. Publishing the live
    // session used to overwrite it, so a cancel that had to escalate
    // past a wedged agent had nothing left to pull and the run stayed
    // `running` with a live process nothing could stop.
    let (session, _agent) = super::super::protocol_tests::connect(Default::default()).await;
    let registry = FlightRegistry::default();
    let flight = registry.register("run-1").expect("registered");
    let (ready, published) = tokio::sync::oneshot::channel();
    let task = tokio::spawn({
        let flight = Arc::clone(&flight);
        async move {
            flight.attach_session(Arc::new(session));
            let _ = ready.send(());
            std::future::pending::<()>().await;
        }
    });
    // Published the way `launch` publishes it, before the session.
    flight.attach_abort(task.abort_handle());
    published.await.expect("the session went live");
    assert!(
        registry.abort("run-1"),
        "the abort handle must survive going live"
    );
    assert!(
        !registry.abort("run-1"),
        "the handle is taken, so a second escalation is inert"
    );
    let _ = task.await;
}
