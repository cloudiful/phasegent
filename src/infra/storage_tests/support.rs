use super::*;

pub(crate) fn unique_temp_dir(label: &str) -> PathBuf {
    crate::test_scratch::root().join(format!(
        "phasegent-storage-{label}-{}-{}",
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

pub(crate) fn open_at_temp(label: &str) -> (PathBuf, Storage) {
    let temp_dir = unique_temp_dir(label);
    let storage = Storage::open_at(&temp_dir.join(DB_FILENAME)).unwrap();
    (temp_dir, storage)
}
