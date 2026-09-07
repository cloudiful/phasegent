//! Contract coverage for the standalone WiX 6.0.2 per-user installer.
//!
//! The MSI keeps one installed exe (CLI `PATH` untouched) and offers two
//! independent shortcut options on their own dialog: Start Menu defaults to
//! checked, Desktop defaults to unchecked. Both shortcuts invoke the
//! installed exe with the literal `gui` argument. These tests pin the file
//! contract without requiring a Windows runner: upgrade identity, explicit
//! `gui` arguments, per-user cleanup markers, the renamed WixUI sequence,
//! and the release workflow compile shape.

use std::path::PathBuf;

fn workspace_file(name: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(name))
        .unwrap_or_else(|err| panic!("read {name}: {err}"))
}

fn phasegent_wxs() -> String {
    workspace_file("wix/phasegent.wxs")
}

fn shortcuts_wxs() -> String {
    workspace_file("wix/Shortcuts.wxs")
}

fn options_dlg_wxs() -> String {
    workspace_file("wix/ShortcutOptionsDlg.wxs")
}

fn release_yml() -> String {
    workspace_file(".github/workflows/release.yml")
}

#[test]
fn installer_preserves_upgrade_and_exe_identity() {
    let wxs = phasegent_wxs();

    assert!(
        wxs.contains("UpgradeCode=\"51047C29-F895-4705-86DC-0A847258CA61\""),
        "UpgradeCode must stay stable for MajorUpgrade; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Id=\"PhasegentExeComponent\"")
            && wxs.contains("Guid=\"3D6E603A-65F6-407B-8201-FAFDAFB8AE2A\""),
        "exe component identity must be preserved; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Id=\"PhasegentExeFile\""),
        "exe file id referenced by shortcuts must be preserved; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Scope=\"perUser\""),
        "installer must stay per-user; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Name=\"PATH\"") && wxs.contains("Part=\"last\""),
        "CLI PATH environment entry must be preserved; got:\n{wxs}"
    );
    assert!(
        wxs.contains("<MajorUpgrade") && wxs.contains("AllowDowngrades=\"no\""),
        "MajorUpgrade downgrade guard must be preserved; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Id=\"MainFeature\""),
        "MainFeature must be preserved; got:\n{wxs}"
    );
    assert!(
        !wxs.contains("tauri") && !wxs.contains("Tauri"),
        "Windows MSI must not convert to the Tauri bundler; got:\n{wxs}"
    );
}

#[test]
fn installer_wires_custom_shortcut_ui_and_components() {
    let wxs = phasegent_wxs();

    assert!(
        wxs.contains("Id=\"WixUI_Phasegent\""),
        "Package must reference the renamed shortcut-options UI set; got:\n{wxs}"
    );
    assert!(
        !wxs.contains("Id=\"WixUI_InstallDir\""),
        "Package must not reference the stock WixUI_InstallDir set; got:\n{wxs}"
    );
    assert!(
        wxs.contains("ComponentGroupRef Id=\"ShortcutComponents\""),
        "MainFeature must include the shortcut components; got:\n{wxs}"
    );
    assert!(
        wxs.contains("ComponentGroupRef Id=\"ProductComponents\""),
        "MainFeature must keep the exe component group; got:\n{wxs}"
    );
}

#[test]
fn shortcut_properties_are_independent_with_documented_defaults() {
    let wxs = shortcuts_wxs();

    assert!(
        wxs.contains("<Property Id=\"PHASEGENT_STARTMENU_SHORTCUT\" Value=\"1\""),
        "Start Menu must default to checked (Value=\"1\"); got:\n{wxs}"
    );
    let desktop_prop = wxs
        .lines()
        .find(|line| line.contains("PHASEGENT_DESKTOP_SHORTCUT"))
        .expect("Desktop property must exist");
    assert!(
        !desktop_prop.contains("Value=\"1\""),
        "Desktop must default to unchecked (no Value=\"1\"); got:\n{desktop_prop}"
    );
    assert!(
        wxs.contains("PHASEGENT_STARTMENU_SHORTCUT=1")
            && wxs.contains("PHASEGENT_DESKTOP_SHORTCUT=1"),
        "each shortcut component needs its own =1 condition; got:\n{wxs}"
    );
}

#[test]
fn shortcuts_invoke_installed_exe_with_gui_and_clean_up() {
    let wxs = shortcuts_wxs();

    assert_eq!(
        wxs.matches("Arguments=\"gui\"").count(),
        2,
        "both shortcuts must pass the literal gui argument; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Target=\"[!PhasegentExeFile]\""),
        "shortcuts must target the installed exe file key; got:\n{wxs}"
    );
    assert!(
        wxs.contains("WorkingDirectory=\"INSTALLFOLDER\""),
        "shortcuts must run with the install folder as working directory; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Advertise=\"no\""),
        "shortcuts must be non-advertised shell links; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Id=\"ApplicationProgramsFolder\""),
        "Start Menu folder directory must exist; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Directory=\"DesktopFolder\""),
        "Desktop component must install to DesktopFolder; got:\n{wxs}"
    );
    assert!(
        wxs.contains("<RemoveFolder Directory=\"ApplicationProgramsFolder\" On=\"uninstall\""),
        "Start Menu folder must be removed on uninstall; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Root=\"HKCU\"") && wxs.matches("KeyPath=\"yes\"").count() >= 2,
        "each per-user shortcut component needs its own HKCU KeyPath; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Guid=\"932CC295-B320-45AC-AE27-45B5115101CE\"")
            && wxs.contains("Guid=\"59C3A30A-67EA-4214-8A19-6D3150F6E363\""),
        "shortcut components need stable distinct GUIDs; got:\n{wxs}"
    );
}

#[test]
fn options_dialog_binds_two_independent_checkboxes() {
    let wxs = options_dlg_wxs();

    assert!(
        wxs.contains("Dialog Id=\"ShortcutOptionsDlg\""),
        "custom dialog must exist; got:\n{wxs}"
    );
    for (control, property) in [
        (
            "StartMenuShortcutCheckBox",
            "PHASEGENT_STARTMENU_SHORTCUT",
        ),
        (
            "DesktopShortcutCheckBox",
            "PHASEGENT_DESKTOP_SHORTCUT",
        ),
    ] {
        assert!(
            wxs.contains(control) && wxs.contains(property),
            "dialog must bind {control} to {property}; got:\n{wxs}"
        );
    }
    assert_eq!(
        wxs.matches("CheckBoxValue=\"1\"").count(),
        2,
        "both checkboxes must set their property to 1; got:\n{wxs}"
    );
}

#[test]
fn options_dialog_uses_wixui_banner_bitmap() {
    let wxs = options_dlg_wxs();

    assert!(
        wxs.contains("Text=\"WixUI_Bmp_Banner\""),
        "BannerBitmap must reference the WixUI_Bmp_Banner binary; got:\n{wxs}"
    );
    assert!(
        !wxs.contains("WixUIBannerBmp"),
        "stale WixUIBannerBmp id must not remain; got:\n{wxs}"
    );
}

#[test]
fn options_dialog_sequence_only_forks_fresh_install() {
    let wxs = options_dlg_wxs();

    assert!(
        wxs.contains("Value=\"ShortcutOptionsDlg\""),
        "InstallDir Next must route to the options dialog; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Dialog=\"ShortcutOptionsDlg\"")
            && wxs.contains("Value=\"VerifyReadyDlg\""),
        "options dialog Next must continue to VerifyReadyDlg; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Value=\"ShortcutOptionsDlg\"")
            && wxs.contains("Condition=\"NOT Installed\""),
        "VerifyReadyDlg Back on fresh install must return to options; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Value=\"MaintenanceTypeDlg\""),
        "maintenance Back navigation must be preserved; got:\n{wxs}"
    );
    assert!(
        wxs.contains("Id=\"WixUI_Phasegent"),
        "renamed WixUI sequence ids must be used; got:\n{wxs}"
    );
    assert!(
        !wxs.contains("Id=\"WixUI_InstallDir\""),
        "custom sequence must not reuse the stock WixUI_InstallDir id; got:\n{wxs}"
    );
}

#[test]
fn release_workflow_compiles_all_wix_sources_without_changing_shape() {
    let yml = release_yml();

    assert!(
        yml.contains(
            "wix build wix/phasegent.wxs wix/Shortcuts.wxs wix/ShortcutOptionsDlg.wxs"
        ),
        "wix build must compile all three sources together; got:\n{yml}"
    );
    assert!(
        yml.contains("wix --version") && yml.contains("WixToolset.UI.wixext/6.0.2"),
        "WiX 6.0.2 plus the UI extension must be preserved; got:\n{yml}"
    );
    assert!(
        yml.contains("x86_64-pc-windows-msvc.msi")
            && yml.contains("x86_64-pc-windows-msvc.exe"),
        "Windows artifact pair (exe plus MSI) must be preserved; got:\n{yml}"
    );
    // Scope the no-zip/no-pdb check to the Windows upload block: cleanup
    // commands legitimately mention *.wixpdb and comments mention the
    // removed portable zip, but those files must never be uploaded.
    let upload_block = yml
        .split("Upload artifact (Windows GUI)")
        .nth(1)
        .expect("Windows upload block must exist")
        .split("- name: Show sccache stats")
        .next()
        .expect("upload block must end before sccache stats");
    assert!(
        !upload_block.contains(".zip") && !upload_block.contains(".wixpdb"),
        "Windows upload must stay exactly exe plus MSI; got:\n{upload_block}"
    );
}
