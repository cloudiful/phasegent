//! Desktop installer contract for the Electron Builder MSI/DMG packages.
//!
//! Electron Builder is the only desktop packager: `electron-builder.yml`
//! carries the app identity, icons, per-user shortcut options, and the
//! per-target Rust companion resource, while `.github/workflows/release.yml`
//! stages that companion and produces the unsigned MSI/DMG. These tests pin the
//! contract without requiring a Windows or macOS runner: installer identity and
//! version parity, shortcut behavior, companion staging, CLI artifact names,
//! and the absence of the retired desktop bundler wiring.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_file(name: &str) -> String {
    let root = workspace_root();
    std::fs::read_to_string(root.join(name)).unwrap_or_else(|err| panic!("read {name}: {err}"))
}

fn builder_yml() -> String {
    workspace_file("electron-builder.yml")
}

fn release_yml() -> String {
    workspace_file(".github/workflows/release.yml")
}

fn package_json() -> String {
    workspace_file("package.json")
}

fn cargo_toml() -> String {
    workspace_file("Cargo.toml")
}

/// Extract a `"version": "x.y.z"` value from a manifest without a JSON parser.
fn json_version(manifest: &str) -> String {
    let start = manifest
        .find("\"version\"")
        .expect("manifest must carry a version field");
    let rest = &manifest[start..];
    let open = rest.find('"').expect("version key is quoted") + 1;
    let rest = &rest[open..];
    let key_end = rest.find('"').expect("version key is closed");
    let rest = &rest[key_end + 1..];
    let value_open = rest.find('"').expect("version value is quoted") + 1;
    let rest = &rest[value_open..];
    let value_end = rest.find('"').expect("version value is closed");
    rest[..value_end].to_owned()
}

fn is_strict_numeric(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

#[test]
fn electron_builder_keeps_desktop_identity_and_version_manifests() {
    let builder = builder_yml();
    let root = workspace_root();

    assert!(
        builder.contains("appId: com.cloud1ful.phasegent"),
        "Electron Builder must keep the existing desktop identity; got:\n{builder}"
    );
    assert!(
        builder.contains("productName: phasegent"),
        "Electron Builder must keep the phasegent product name; got:\n{builder}"
    );
    assert!(
        builder.contains("output: dist/electron") && builder.contains("asar: true"),
        "Electron Builder output dir and asar packaging must stay pinned; got:\n{builder}"
    );
    assert!(
        builder.contains("electron/dist/**")
            && builder.contains("frontend/dist/**")
            && builder.contains("package.json"),
        "the packaged app must embed the Electron bundles, renderer dist, and manifest; got:\n{builder}"
    );
    // `electron-builder` must not grow a runtime dependency tree: the renderer
    // is bundled by Vite and the main/preload bundles are self-contained.
    assert!(
        builder.contains("!node_modules/**"),
        "packaging must not ship a runtime node_modules tree; got:\n{builder}"
    );

    // One upgrade identity across the Rust shell and the packaged app.
    let gui_mod = std::fs::read_to_string(root.join("src/gui/mod.rs"))
        .unwrap_or_else(|err| panic!("read src/gui/mod.rs: {err}"));
    assert!(
        gui_mod.contains("identifier: \"com.cloud1ful.phasegent\""),
        "Rust app metadata must keep the packaged appId; got:\n{gui_mod}"
    );

    // Electron Builder derives the MSI/DMG version from `package.json`, and
    // the binary `--version` flows from `Cargo.toml`; both must match and stay
    // strict numeric so the MSI accepts them.
    let cargo_version = cargo_toml();
    let package_version = json_version(&package_json());
    assert!(
        cargo_version.contains(&format!("version = \"{package_version}\"")),
        "Cargo.toml must match package.json version {package_version};\n{cargo_version}"
    );
    assert!(
        is_strict_numeric(&package_version),
        "packaged installer version must be strict MAJOR.MINOR.PATCH, got {package_version}"
    );
}

#[test]
fn electron_builder_preserves_msi_and_dmg_targets_with_icons() {
    let builder = builder_yml();

    assert!(
        builder.contains("icon: icons/icon.icns") && builder.contains("- dmg"),
        "macOS must keep the DMG target with its icns icon; got:\n{builder}"
    );
    assert!(
        builder.contains("icon: icons/icon.ico") && builder.contains("- msi"),
        "Windows must keep the MSI target with its ico icon; got:\n{builder}"
    );
    assert!(
        builder.contains("category: public.app-category.developer-tools"),
        "macOS app category must stay pinned; got:\n{builder}"
    );
    assert!(
        builder.contains("title: phasegent"),
        "the DMG volume title must stay phasegent; got:\n{builder}"
    );
}

#[test]
fn electron_builder_stages_the_rust_companion_per_target() {
    let builder = builder_yml();

    assert!(
        builder.contains("from: target/companion/phasegent")
            && builder.contains("to: phasegent-backend"),
        "macOS must stage the companion backend resource; got:\n{builder}"
    );
    assert!(
        builder.contains("from: target/companion/phasegent.exe")
            && builder.contains("to: phasegent-backend.exe"),
        "Windows must stage the companion backend resource; got:\n{builder}"
    );
}

#[test]
fn electron_builder_preserves_per_user_shortcut_options() {
    let builder = builder_yml();

    assert!(
        builder.contains("perMachine: false"),
        "the MSI must stay per-user; got:\n{builder}"
    );
    for required in [
        "shortcutName: phasegent",
        "createDesktopShortcut: true",
        "createStartMenuShortcut: true",
        "runAfterFinish: true",
    ] {
        assert!(
            builder.contains(required),
            "MSI shortcut option {required:?} must be preserved; got:\n{builder}"
        );
    }
}

#[test]
fn electron_builder_keeps_the_desktop_release_unsigned() {
    let builder = builder_yml();

    assert!(
        builder.contains("identity: null"),
        "macOS packaging must not assume a signing identity; got:\n{builder}"
    );
    for forbidden in ["CSC_LINK", "CSC_KEY_PASSWORD", "APPLE_ID", "WIN_CSC_LINK"] {
        assert!(
            !builder.contains(forbidden),
            "packaging config must not embed signing credentials ({forbidden}); got:\n{builder}"
        );
    }
}

#[test]
fn release_workflow_packages_desktop_with_electron_builder_only() {
    let yml = release_yml();

    for required in [
        "bun scripts/electron-build.mjs",
        "bun run validate",
        "bun scripts/electron-backend.mjs --from target/${{ matrix.target }}/release/phasegent",
        "bun x electron-builder --config electron-builder.yml --mac dmg",
        "bun x electron-builder --config electron-builder.yml --win msi",
        "bun scripts/dist-hash.mjs",
        "PHASEGENT_FRONTEND_DIST_HASH",
        "--publish never",
    ] {
        assert!(
            yml.contains(required),
            "release workflow must contain {required:?}; got:\n{yml}"
        );
    }
    // The former desktop packagers must be gone.
    for forbidden in [
        "tauri",
        "Tauri",
        "wix build",
        "WixToolset",
        "Shortcuts.wxs",
        "--features gui",
        "bundle/dmg",
    ] {
        assert!(
            !yml.contains(forbidden),
            "release workflow must not contain {forbidden:?}; got:\n{yml}"
        );
    }
    // The packaged companion must be verified as an actual resource of the app.
    assert!(
        yml.contains("phasegent.app/Contents/Resources/phasegent-backend")
            || yml.contains("Contents/Resources/phasegent-backend"),
        "macOS packaging must verify the companion inside the .app; got:\n{yml}"
    );
    assert!(
        yml.contains("win-unpacked/resources/phasegent-backend.exe"),
        "Windows packaging must verify the companion inside the app; got:\n{yml}"
    );
}

#[test]
fn release_workflow_keeps_cli_artifact_names_and_skips_sidecars() {
    let yml = release_yml();

    for required in [
        "phasegent-${{ github.ref_name }}-${{ matrix.target }}.exe",
        "phasegent-${{ github.ref_name }}-${{ matrix.target }}.msi",
        "phasegent-${{ github.ref_name }}-${{ matrix.target }}.dmg",
        "cargo build --release --bin phasegent --target ${{ matrix.target }}",
        "--features postgres,notify-dingtalk,notify-email",
    ] {
        assert!(
            yml.contains(required),
            "release workflow must contain {required:?}; got:\n{yml}"
        );
    }

    // Scope the no-zip/no-pdb check to the Windows upload block: cleanup
    // commands may mention sidecars, but those files must never be uploaded.
    let upload_block = yml
        .split("Upload artifact (Windows desktop)")
        .nth(1)
        .expect("Windows upload block must exist")
        .split("- name: Show sccache stats")
        .next()
        .expect("upload block must end before sccache stats");
    assert!(
        !upload_block.contains(".zip") && !upload_block.contains(".wixpdb"),
        "Windows upload must stay exactly exe plus MSI; got:\n{upload_block}"
    );
    assert!(
        upload_block.contains(".exe") && upload_block.contains(".msi"),
        "Windows upload must publish the exe plus MSI pair; got:\n{upload_block}"
    );

    let mac_upload_block = yml
        .split("Upload artifact (macOS desktop)")
        .nth(1)
        .expect("macOS upload block must exist")
        .split("- name: Upload artifact (Windows desktop)")
        .next()
        .expect("macOS upload block must end before the Windows block");
    assert!(
        mac_upload_block.contains(".dmg"),
        "macOS upload must publish the DMG; got:\n{mac_upload_block}"
    );
}

#[test]
fn release_workflow_gates_publishing_on_strict_version_parity() {
    let yml = release_yml();

    assert!(
        yml.contains("version-check:") && yml.contains("needs: version-check"),
        "the release build must depend on the version check; got:\n{yml}"
    );
    assert!(
        yml.contains("tomllib") && yml.contains("package.json") && yml.contains("Cargo.toml"),
        "the version check must compare both manifests; got:\n{yml}"
    );
    assert!(
        yml.contains(r"v(\d+\.\d+\.\d+)") && yml.contains("MAJOR.MINOR.PATCH"),
        "the version check must enforce a strict numeric tag; got:\n{yml}"
    );
    assert!(
        !yml.contains("package.json.outputs") && !yml.contains("version:bump"),
        "the workflow must never bump a version; got:\n{yml}"
    );
}

#[test]
fn legacy_wix_desktop_installer_sources_are_removed() {
    let root = workspace_root();
    for source in [
        "wix/phasegent.wxs",
        "wix/Shortcuts.wxs",
        "wix/ShortcutOptionsDlg.wxs",
    ] {
        assert!(
            !Path::new(&root.join(source)).exists(),
            "the legacy WiX installer source {source} must be removed in favour of Electron Builder"
        );
    }
    // No WiX source may remain anywhere under the retired installer directory.
    assert!(
        !root.join("wix").exists(),
        "the retired wix/ directory must be removed with its sources"
    );
    let yml = release_yml();
    for forbidden in [
        "wix/",
        "wix build",
        "WixToolset",
        "Shortcuts.wxs",
        "ShortcutOptionsDlg.wxs",
    ] {
        assert!(
            !yml.contains(forbidden),
            "the release workflow must not reference the retired WiX build ({forbidden}); got:\n{yml}"
        );
    }
}
