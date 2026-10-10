//! Native CLI release workflow: target matrix, build features, artifact names,
//! checksum publication, and the absence of any desktop packaging.

use std::path::PathBuf;

const RELEASE_TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

fn release_yml() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(".github/workflows/release.yml"))
        .unwrap_or_else(|err| panic!("read release workflow: {err}"))
}

#[test]
fn release_builds_every_native_target_with_the_full_feature_set() {
    let yml = release_yml();
    for target in RELEASE_TARGETS {
        assert!(
            yml.contains(&format!("target: {target}")),
            "release matrix must build {target}; got:\n{yml}"
        );
    }
    assert!(
        yml.contains(
            "cargo build --release --bin phasegent --target ${{ matrix.target }} --features postgres,notify-dingtalk,notify-email"
        ),
        "every release binary must build with the full feature set; got:\n{yml}"
    );
}

#[test]
fn release_publishes_raw_native_binaries_with_checksums() {
    let yml = release_yml();
    for artifact in [
        "cp target/${{ matrix.target }}/release/phasegent phasegent-${{ github.ref_name }}-${{ matrix.target }}",
        "cp target/${{ matrix.target }}/release/phasegent.exe phasegent-${{ github.ref_name }}-${{ matrix.target }}.exe",
    ] {
        assert!(
            yml.contains(artifact),
            "release must stage the raw artifact {artifact:?}; got:\n{yml}"
        );
    }
    assert!(
        yml.contains("sha256sum phasegent-* > SHA256SUMS"),
        "release must checksum the staged artifacts into SHA256SUMS; got:\n{yml}"
    );
    for published in [
        "dist/phasegent-*-x86_64-unknown-linux-gnu",
        "dist/phasegent-*-aarch64-unknown-linux-gnu",
        "dist/phasegent-*-aarch64-apple-darwin",
        "dist/phasegent-*-x86_64-pc-windows-msvc.exe",
        "dist/SHA256SUMS",
    ] {
        assert!(
            yml.contains(published),
            "release must publish {published}; got:\n{yml}"
        );
    }
}

#[test]
fn release_gates_publishing_on_strict_numeric_version_parity() {
    let yml = release_yml();
    assert!(
        yml.contains("version-check:") && yml.contains("needs: version-check"),
        "the release build must depend on the version check; got:\n{yml}"
    );
    assert!(
        yml.contains(r"v(\d+\.\d+\.\d+)") && yml.contains("MAJOR.MINOR.PATCH"),
        "the version check must enforce a strict numeric tag; got:\n{yml}"
    );
    assert!(
        yml.contains("tomllib") && yml.contains("package.json") && yml.contains("Cargo.toml"),
        "the version check must compare both manifests; got:\n{yml}"
    );
    assert!(
        !yml.contains("version:bump"),
        "the workflow must never bump a version; got:\n{yml}"
    );
}

#[test]
fn release_has_no_desktop_packaging_surface() {
    let yml = release_yml();
    // Desktop tooling and renderer paths must not appear anywhere, including
    // as whole words in comments. Matching is per whitespace-separated token
    // so runner names such as `ubuntu-24.04` (which contains "bun") pass.
    let forbidden_tokens = [
        "bun",
        "electron",
        "electron-builder",
        "dmg",
        "msi",
        "desktop",
        "dist-hash",
        "PHASEGENT_FRONTEND_DIST_HASH",
    ];
    for token in yml.split_whitespace() {
        let token =
            token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_');
        for forbidden in forbidden_tokens {
            assert!(
                !token.eq_ignore_ascii_case(forbidden),
                "the release workflow must not reference the desktop surface ({forbidden}); got:\n{yml}"
            );
        }
    }
    for forbidden_path in ["frontend/dist", "--features gui", "setup-bun"] {
        assert!(
            !yml.contains(forbidden_path),
            "the release workflow must not reference {forbidden_path}; got:\n{yml}"
        );
    }
}
