use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn workflow_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Acquire the workflow test lock while tolerating a previous
/// panic that poisoned it. The mirror plugin contract tests and
/// the storage non-persistence test both mutate
/// `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` and
/// `PHASEGENT_REDMINE_REPOSITORY_URL`; without this mutex the
/// two test groups can race under the default `cargo test`
/// parallel runner and the contract test's bearer-key assertion
/// would observe a value installed by the storage test.
pub(crate) fn lock_workflow_tests() -> MutexGuard<'static, ()> {
    let mutex = workflow_test_lock();
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// RAII guard that sets an environment variable for the duration
/// of its scope and restores the previous value on Drop. Tests
/// that mutate process-wide state must use this helper so they
/// leave the host shell with the same environment they found.
pub(crate) struct EnvGuard {
    name: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    /// Capture the current value of `name` (if any), install
    /// `value`, and return a guard whose Drop restores the
    /// original state. The previous value is never copied into
    /// the test output.
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        let previous = env::var_os(name);
        // SAFETY::`set_var`/`remove_var` are unsafe in this
        // toolchain; tests serialise on `lock_workflow_tests()`
        // so no other thread can observe the transient state.
        unsafe {
            env::set_var(name, value);
        }
        Self { name, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY::Symmetric to `set_var` above; the lock guard
        // is still held when the test stack unwinds.
        unsafe {
            if let Some(previous) = self.previous.take() {
                env::set_var(self.name, previous);
            } else {
                env::remove_var(self.name);
            }
        }
    }
}
