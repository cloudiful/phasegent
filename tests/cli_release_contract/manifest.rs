//! Version mirror and feature wiring for the CLI release.

use std::path::PathBuf;

fn workspace_file(name: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(name)).unwrap_or_else(|err| panic!("read {name}: {err}"))
}

/// The `[package] version` value from `Cargo.toml`.
fn cargo_package_version() -> String {
    let manifest: toml::Table = workspace_file("Cargo.toml")
        .parse()
        .expect("Cargo.toml must be valid TOML");
    manifest["package"]["version"]
        .as_str()
        .expect("[package] version must be a string")
        .to_owned()
}

#[test]
fn package_json_version_mirrors_cargo_package_version() {
    let package = workspace_file("package.json");
    let mirror = package
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("\"version\": \"")
                .and_then(|rest| rest.strip_suffix("\","))
                .or_else(|| {
                    trimmed
                        .strip_prefix("\"version\": \"")
                        .and_then(|rest| rest.strip_suffix('"'))
                })
        })
        .expect("package.json must carry a top-level version");
    assert_eq!(mirror, cargo_package_version());
}

/// The release binaries are built with exactly this feature set; each name must
/// be a declared key of the `[features]` table, not only a workflow string.
#[test]
fn release_feature_set_is_declared_in_cargo_features() {
    let manifest: toml::Table = workspace_file("Cargo.toml")
        .parse()
        .expect("Cargo.toml must be valid TOML");
    let features = manifest["features"]
        .as_table()
        .expect("Cargo.toml must declare a [features] table");
    for feature in ["postgres", "notify-dingtalk", "notify-email"] {
        assert!(
            features.contains_key(feature),
            "Cargo.toml must declare the release feature {feature}"
        );
    }
}
