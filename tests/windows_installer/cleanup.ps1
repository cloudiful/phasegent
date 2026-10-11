$ErrorActionPreference = 'Stop'
$checker = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../scripts/check-windows-msi.ps1'))
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($checker, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) {
    throw "MSI checker has PowerShell parse errors: $($parseErrors[0].Message)"
}
$validator = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -ceq 'Assert-PhasegentRemoveFileRows'
}, $true))
if ($validator.Count -ne 1) {
    throw "Expected one RemoveFile row validator; found $($validator.Count)."
}
$validatorScript = [scriptblock]::Create($validator[0].Extent.Text)
. $validatorScript

function New-Row {
    param(
        [string]$FileKey,
        [string]$DirProperty,
        [string]$Component = 'PhasegentCli',
        [string]$FileName = '',
        [int]$InstallMode = 2,
        [bool]$FileNameIsNull = $true
    )
    [pscustomobject]@{
        FileKey = $FileKey
        Component_ = $Component
        FileName = $FileName
        FileNameIsNull = $FileNameIsNull
        DirProperty = $DirProperty
        InstallMode = $InstallMode
    }
}

function New-SafeRows {
    @(
        (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER'),
        (New-Row 'RemoveProgramsFolder' 'ProgramsFolder')
    )
}

function Assert-Rejected {
    param([string]$Name, [object[]]$Rows)
    $rejected = $false
    try {
        Assert-PhasegentRemoveFileRows -Rows $Rows -ComponentId 'PhasegentCli'
    } catch {
        $rejected = $true
    }
    if (-not $rejected) {
        throw "Unsafe RemoveFile contract was accepted: $Name"
    }
}

$safeRows = New-SafeRows
Assert-PhasegentRemoveFileRows -Rows $safeRows -ComponentId 'PhasegentCli'

Assert-Rejected 'wrong directory' @(
    (New-Row 'RemoveInstallFolder' 'LocalAppDataFolder'),
    (New-Row 'RemoveProgramsFolder' 'ProgramsFolder')
)
Assert-Rejected 'wrong component' @(
    (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER' 'OtherComponent'),
    (New-Row 'RemoveProgramsFolder' 'ProgramsFolder')
)
Assert-Rejected 'file or wildcard pattern' @(
    (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER' -FileName '*.cfg' -FileNameIsNull $false),
    (New-Row 'RemoveProgramsFolder' 'ProgramsFolder')
)
Assert-Rejected 'wrong install mode' @(
    (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER' 'PhasegentCli' '' 1),
    (New-Row 'RemoveProgramsFolder' 'ProgramsFolder')
)
Assert-Rejected 'extra row' (@(New-SafeRows) + (New-Row 'Extra' 'INSTALLFOLDER'))
Assert-Rejected 'duplicate row' @(
    (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER'),
    (New-Row 'RemoveInstallFolder' 'INSTALLFOLDER')
)
