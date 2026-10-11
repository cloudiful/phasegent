use std::path::PathBuf;
use std::process::Command;

// The MSI SQL contract lives in PowerShell because the escaping is a runtime
// string property. These assertions localize the expectation so a silent
// un-escaping of the checker cannot pass unnoticed.
const ESCAPED_QUERIES: [(&str, &str); 10] = [
    (
        "SELECT `Property`, `Value` FROM `Property`",
        "@('Property', 'Value')",
    ),
    (
        "SELECT `Component`, `ComponentId`, `Directory_`, `Attributes`, `KeyPath` FROM `Component`",
        "@('Component', 'ComponentId', 'Directory_', 'Attributes', 'KeyPath') @(4)",
    ),
    (
        "SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`",
        "@('Directory', 'Directory_Parent', 'DefaultDir')",
    ),
    (
        "SELECT `File`, `Component_`, `FileName` FROM `File`",
        "@('File', 'Component_', 'FileName')",
    ),
    (
        "SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`",
        "@('Registry', 'Root', 'Key', 'Name', 'Value', 'Component_') @(2)",
    ),
    (
        "SELECT `Environment`, `Name`, `Value`, `Component_` FROM `Environment`",
        "@('Environment', 'Name', 'Value', 'Component_')",
    ),
    (
        "SELECT `FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`",
        "@('FileKey', 'Component_', 'FileName', 'DirProperty', 'InstallMode') @(5) @(3)",
    ),
    (
        "SELECT `UpgradeCode`, `VersionMin`, `VersionMax`, `Attributes`, `ActionProperty` FROM `Upgrade`",
        "@('UpgradeCode', 'VersionMin', 'VersionMax', 'Attributes', 'ActionProperty') @(4)",
    ),
    (
        "SELECT `Condition`, `Description` FROM `LaunchCondition`",
        "@('Condition', 'Description')",
    ),
    ("SELECT `Name` FROM `_Tables`", "@('Name')"),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn every_checker_query_escapes_its_table_and_columns() {
    let checker = read("scripts/check-windows-msi.ps1");
    for (query, columns) in ESCAPED_QUERIES {
        assert!(
            checker.contains(&format!(
                "Read-MsiRows '{query}' {columns} -Database $database"
            )),
            "checker must send the escaped query {query} with its column mapping {columns}"
        );
    }
    assert_eq!(
        ESCAPED_QUERIES.len(),
        checker.matches("Read-MsiRows 'SELECT").count(),
        "the checker must author exactly one query per expected MSI table"
    );
    // The reserved-word collision that broke the native OpenView call.
    assert!(
        !checker.contains("SELECT Registry, Root, Key, Name, Value, Component_ FROM Registry"),
        "the unquoted Registry query must never reach OpenView"
    );
    for unescaped in [
        "SELECT `Property`, `Value` FROM Property",
        "SELECT `Name` FROM _Tables",
    ] {
        assert!(
            !checker.contains(unescaped),
            "identifier escaping must cover table names: {unescaped}"
        );
    }
}

#[test]
fn escaped_query_contract_runs_when_powershell_is_available() {
    let tests = read("tests/windows_installer/queries.ps1");
    for contract in [
        "System.Management.Automation.Language.Parser]::ParseFile",
        "StringConstantExpressionAst",
        "It is NOT a native MSI SQL",
        "unexpected OpenView query",
        "query is not escaped per MSI SQL syntax",
    ] {
        assert!(
            tests.contains(contract),
            "missing query contract guard {contract:?}"
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
            .arg(root().join("tests/windows_installer/queries.ps1"))
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("run {program}: {error}"),
        };
        assert!(
            output.status.success(),
            "MSI query contract tests failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    eprintln!(
        "PowerShell unavailable; Windows MSI query contract deferred to the Windows workflow."
    );
}
