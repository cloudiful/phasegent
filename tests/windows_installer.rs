use std::path::PathBuf;
use std::process::Command;

#[path = "windows_installer/rows_contract.rs"]
mod rows_contract;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn wix_source_authors_one_per_user_x64_cli_component() {
    const CHECK_XML: &str = r#"
import sys
import xml.etree.ElementTree as ET

ns = {"w": "http://wixtoolset.org/schemas/v4/wxs"}
root = ET.parse(sys.argv[1]).getroot()
assert root.tag == "{http://wixtoolset.org/schemas/v4/wxs}Wix"
package = root.find("w:Package", ns)
assert package is not None
assert package.attrib["Name"] == "phasegent CLI"
assert package.attrib["Manufacturer"] == "phasegent"
assert package.attrib["Version"] == "$(var.Version)"
assert package.attrib["UpgradeCode"] == "6EC9BF6B-62FB-4464-BC19-B5AD6A8AF08A"
assert package.attrib["Scope"] == "perUser"
assert package.attrib["Compressed"] == "yes"
assert package.find("w:MediaTemplate", ns).attrib["EmbedCab"] == "yes"

upgrade = package.find("w:MajorUpgrade", ns)
assert upgrade is not None
assert upgrade.attrib["AllowDowngrades"] == "no"
assert "DowngradeErrorMessage" in upgrade.attrib

standard = root.find(".//w:StandardDirectory", ns)
assert standard.attrib["Id"] == "LocalAppDataFolder"
programs = standard.find("w:Directory", ns)
assert programs.attrib == {"Id": "ProgramsFolder", "Name": "Programs"}
install = programs.find("w:Directory", ns)
assert install.attrib == {"Id": "INSTALLFOLDER", "Name": "phasegent-cli"}

group = root.find(".//w:ComponentGroup", ns)
assert group.attrib == {"Id": "PhasegentCliComponents", "Directory": "INSTALLFOLDER"}
components = group.findall("w:Component", ns)
assert len(components) == 1
component = components[0]
assert component.attrib == {
    "Id": "PhasegentCli", "Guid": "36547836-2B46-42AF-B453-E29B2F5AF2C1"
}
files = component.findall("w:File", ns)
assert len(files) == 1
assert files[0].attrib == {
    "Id": "PhasegentExe", "Source": "$(var.SourceExe)", "KeyPath": "no"
}
registry = component.find("w:RegistryValue", ns)
assert registry.attrib == {
    "Id": "PhasegentInstallPath", "Root": "HKCU",
    "Key": "Software\\phasegent\\phasegent-cli", "Name": "InstallPath",
    "Type": "string", "Value": "[INSTALLFOLDER]", "KeyPath": "yes"
}
environment = component.find("w:Environment", ns)
assert environment.attrib == {
    "Id": "PhasegentUserPath", "Name": "PATH", "Value": "[INSTALLFOLDER]",
    "Action": "set", "Part": "last", "System": "no", "Permanent": "no"
}
removals = component.findall("w:RemoveFolder", ns)
assert len(removals) == 2
assert {
    (row.attrib["Id"], row.attrib["Directory"], row.attrib["On"])
    for row in removals
} == {
    ("RemoveInstallFolder", "INSTALLFOLDER", "uninstall"),
    ("RemoveProgramsFolder", "ProgramsFolder", "uninstall")
}
assert package.find("w:Feature/w:ComponentGroupRef", ns).attrib["Id"] == "PhasegentCliComponents"
for forbidden in ("Shortcut", "CustomAction", "RemoveRegistry"):
    assert not root.findall(".//w:" + forbidden, ns), forbidden
"#;

    let source = root().join("wix/phasegent.wxs");
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("py", &["-3"]), ("python", &[])]
    } else {
        &[("python3", &[]), ("python", &[])]
    };
    let mut parsed = false;
    for (program, prefix) in candidates {
        let output = match Command::new(program)
            .args(*prefix)
            .arg("-c")
            .arg(CHECK_XML)
            .arg(&source)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("run {program}: {error}"),
        };
        assert!(
            output.status.success(),
            "Python XML contract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        parsed = true;
        break;
    }
    assert!(
        parsed,
        "Python 3 stdlib is required to parse the WiX source."
    );
}

#[test]
fn build_script_gates_version_and_payload_before_wix() {
    let script = read("scripts/build-windows-msi.ps1");
    for contract in [
        "^v(?<version>\\d+\\.\\d+\\.\\d+)$",
        "$limits = @(255, 255, 65535)",
        "Cargo.toml",
        "package.json",
        "-cne $version",
        "Get-Item -LiteralPath $source).Length -le 0",
        "Get-Command wix -ErrorAction Stop",
        "build -arch x64 -d \"Version=$version\" -d \"SourceExe=$source\"",
        "if ($LASTEXITCODE -ne 0)",
        "check-windows-msi.ps1",
    ] {
        assert!(
            script.contains(contract),
            "missing build contract {contract:?}"
        );
    }
}

#[test]
fn msi_checker_opens_the_package_read_only_and_checks_owned_tables() {
    let script = read("scripts/check-windows-msi.ps1");
    for contract in [
        "OpenDatabase($fullPath, 0)",
        "SummaryInformation(0)",
        "SELECT Component, ComponentId, Directory_, Attributes, KeyPath FROM Component",
        "SELECT Directory, Directory_Parent, DefaultDir FROM Directory",
        "SELECT File, Component_, FileName FROM File",
        "SELECT Registry, Root, Key, Name, Value, Component_ FROM Registry",
        "SELECT Environment, Name, Value, Component_ FROM Environment",
        "SELECT FileKey, Component_, FileName, DirProperty, InstallMode FROM RemoveFile",
        "Assert-PhasegentRemoveFileRows -Rows $removeFileRows -ComponentId $pathComponent",
        "@(5) @(3) -Database $database)",
        "$row.FileNameIsNull -ne $true",
        "WIX_UPGRADE_DETECTED",
        "WIX_DOWNGRADE_DETECTED",
        "NOT WIX_DOWNGRADE_DETECTED",
        "ALLUSERS",
        "CustomAction",
    ] {
        assert!(
            script.contains(contract),
            "missing MSI inspection contract {contract:?}"
        );
    }
    assert!(
        !script.contains("msiexec"),
        "the checker must not install the MSI"
    );
    assert!(
        !script.contains("Set-ItemProperty"),
        "the checker must not mutate registry state"
    );
}

#[test]
fn checker_remove_file_table_contract_accepts_only_owned_empty_directories() {
    let checker = read("scripts/check-windows-msi.ps1");
    for contract in [
        "RemoveInstallFolder = 'INSTALLFOLDER'",
        "RemoveProgramsFolder = 'ProgramsFolder'",
        "$Rows.Count -ne $expected.Count",
        "[string]$row.Component_ -cne $ComponentId",
        "$row.FileNameIsNull -ne $true",
        "[string]$row.DirProperty -cne $expected[$fileKey]",
        "[int]$row.InstallMode -ne 2",
    ] {
        assert!(
            checker.contains(contract),
            "missing cleanup contract {contract:?}"
        );
    }
    let tests = read("tests/windows_installer/cleanup.ps1");
    for rejected in [
        "wrong directory",
        "wrong component",
        "file or wildcard pattern",
        "wrong install mode",
        "extra row",
        "duplicate row",
        "Parser]::ParseFile($checker",
    ] {
        assert!(
            tests.contains(rejected),
            "missing rejection case {rejected:?}"
        );
    }

    let candidates: &[&str] = if cfg!(windows) {
        &["pwsh", "powershell"]
    } else {
        &["pwsh"]
    };
    for program in candidates {
        let output = match Command::new(program)
            .arg("-NoProfile")
            .arg("-File")
            .arg(root().join("tests/windows_installer/cleanup.ps1"))
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("run {program}: {error}"),
        };
        assert!(
            output.status.success(),
            "RemoveFile table contract tests failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
}
