//! Native CLI release workflow: target matrix, build features, artifact names,
//! Windows-only MSI publication, checksums, and the absence of desktop packaging.

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
fn release_publishes_raw_native_binaries_and_windows_msi_with_checksums() {
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
        "dist/phasegent-*-x86_64-pc-windows-msvc.msi",
        "dist/SHA256SUMS",
    ] {
        assert!(
            yml.contains(published),
            "release must publish {published}; got:\n{yml}"
        );
    }
}

#[test]
fn windows_msi_is_built_checked_and_uploaded_only_on_the_windows_runner() {
    let yml = release_yml();
    for step in [
        "name: Install pinned WiX CLI\n        if: runner.os == 'Windows'\n        run: dotnet tool install --global wix --version 6.0.2",
        "name: Build and validate Windows MSI\n        if: runner.os == 'Windows'\n        shell: pwsh",
        "name: Upload artifact (Windows MSI)\n        if: runner.os == 'Windows'",
        r#"-OutputPath "phasegent-$($env:GITHUB_REF_NAME)-${{ matrix.target }}.msi""#,
        "path: phasegent-${{ github.ref_name }}-${{ matrix.target }}.msi",
    ] {
        assert!(
            yml.contains(step),
            "Windows MSI release contract must include {step:?}; got:\n{yml}"
        );
    }
    let regression_step = "name: Test Windows MSI PowerShell regressions\n        if: runner.os == 'Windows'\n        shell: pwsh\n        run: |\n          pwsh -NoProfile -File tests/windows_installer/rows.ps1\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n          pwsh -NoProfile -File tests/windows_installer/cleanup.ps1\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }";
    let regression_index = yml
        .find(regression_step)
        .expect("Windows release must run the MSI PowerShell regressions");
    let msi_build_index = yml
        .find("name: Build and validate Windows MSI")
        .expect("Windows release must retain the real MSI build and validation");
    assert!(
        regression_index < msi_build_index,
        "MSI regression scripts must run before the real MSI build"
    );
    assert!(
        yml.contains("dist/phasegent-*-x86_64-pc-windows-msvc.msi"),
        "release must publish only the named Windows MSI artifact; got:\n{yml}"
    );
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
        "pkg",
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
