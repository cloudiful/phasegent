use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn checker_uses_the_shared_noisy_output_safe_row_reader() {
    let checker = read("scripts/check-windows-msi.ps1");
    assert!(checker.contains(". (Join-Path $PSScriptRoot 'windows-msi/rows.ps1')"));
    assert!(!checker.contains("function Read-MsiRows"));
    let query_count = checker.matches("Read-MsiRows 'SELECT").count();
    assert_eq!(query_count, 10, "all ten MSI queries use the shared reader");
    assert_eq!(
        query_count,
        checker.matches("-Database $database").count(),
        "every MSI query passes its opened read-only database"
    );

    let reader = read("scripts/windows-msi/rows.ps1");
    for contract in [
        "function Read-MsiRows",
        "[void]$view.Execute()",
        "[void]$view.Close()",
        "$properties = [ordered]@{}",
        "$rows.Add([pscustomobject]$properties)",
        "[int]$record.IntegerData($ordinal)",
        "[bool]$record.IsNull($ordinal)",
        "Failed to read MSI rows using query [$Query]",
        "foreach ($row in $rows) { Write-Output $row }",
    ] {
        assert!(
            reader.contains(contract),
            "missing row-reader contract {contract:?}"
        );
    }
    assert!(!reader.contains("return $rows.ToArray()"));
}

#[test]
fn fake_msi_rows_regression_runs_when_powershell_is_available() {
    let tests = read("tests/windows_installer/rows.ps1");
    assert!(tests.contains(". (Join-Path $PSScriptRoot '../../scripts/windows-msi/rows.ps1')"));
    for contract in [
        "ExecuteNoise",
        "CloseNoise",
        "empty query row count",
        "single query row count",
        "multiple query row count",
        "empty MSI value must remain present",
        "integer columns remain typed",
        "null flags remain typed",
        "Execute errors include query context",
        "Fetch errors include query context",
        "Close errors include query context",
        "row-field errors include query context",
    ] {
        assert!(
            tests.contains(contract),
            "missing row regression {contract:?}"
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
            .arg(root().join("tests/windows_installer/rows.ps1"))
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("run {program}: {error}"),
        };
        assert!(
            output.status.success(),
            "MSI row contract tests failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    eprintln!(
        "PowerShell unavailable; Windows MSI row regression deferred to the Windows workflow."
    );
}
