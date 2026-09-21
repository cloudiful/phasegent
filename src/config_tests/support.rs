use super::*;

pub(crate) fn unique_temp_dir(label: &str) -> PathBuf {
    crate::test_scratch::root().join(format!(
        "phasegent-config-{label}-{}-{}",
        std::process::id(),
        system_time_nanos()
    ))
}

pub(crate) fn system_time_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn unique_temp_db_path(label: &str) -> PathBuf {
    unique_temp_dir(label).join(DB_FILENAME)
}

pub(crate) fn with_isolated_storage<T>(label: &str, f: impl FnOnce(&Path, &Storage) -> T) -> T {
    let _lock = lock_workflow_tests();
    let db_path = unique_temp_db_path(label);
    let storage = Storage::open_at(&db_path).unwrap();
    let result = f(&db_path, &storage);
    let _ = fs::remove_dir_all(db_path.parent().unwrap());
    result
}

pub(crate) fn toml_temp_paths(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = unique_temp_dir(label);
    (dir.join(DB_FILENAME), dir.join("phasegent.toml"), dir)
}

pub(crate) fn write_toml_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Remove env vars for the test lifetime and restore host values on drop.
/// Needed because `PHASEGENT_PROVIDER`, `PHASEGENT_API_BASE`,
/// `PHASEGENT_REPOSITORY`, `PHASEGENT_REDMINE_API_BASE`,
/// `PHASEGENT_GITLAB_API_BASE`, and close-status vars treat a blank
/// value as a present value (unlike the trimmed globals); tests must
/// remove them rather than set them to `""` to simulate "unset".
pub(crate) struct EnvRemoveGuard {
    pub(crate) saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvRemoveGuard {
    pub(crate) fn remove(names: &[&'static str]) -> Self {
        let mut saved = Vec::with_capacity(names.len());
        for &name in names {
            saved.push((name, std::env::var_os(name)));
            // SAFETY: serialised by `lock_workflow_tests`.
            unsafe {
                std::env::remove_var(name);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvRemoveGuard {
    fn drop(&mut self) {
        // SAFETY: symmetric with `remove`; lock still held on unwind.
        unsafe {
            for (name, previous) in self.saved.drain(..) {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
