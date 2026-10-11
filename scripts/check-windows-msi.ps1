[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$MsiPath,
    [Parameter(Mandatory = $true)]
    [string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$expectedUpgradeCode = '6EC9BF6B-62FB-4464-BC19-B5AD6A8AF08A'
$expectedComponentCode = '36547836-2B46-42AF-B453-E29B2F5AF2C1'
$pathComponent = 'PhasegentCli'

function Assert-PhasegentRemoveFileRows {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Rows,
        [Parameter(Mandatory = $true)]
        [string]$ComponentId
    )

    $expected = @{ RemoveInstallFolder = 'INSTALLFOLDER'; RemoveProgramsFolder = 'ProgramsFolder' }
    if ($Rows.Count -ne $expected.Count) { throw "Expected exactly two owned empty-directory removal rows; found $($Rows.Count)." }

    $seen = @{}
    foreach ($row in $Rows) {
        $fileKey = [string]$row.FileKey
        if (@($expected.Keys) -cnotcontains $fileKey -or $seen.ContainsKey($fileKey)) { throw "Unexpected or duplicate RemoveFile key: $fileKey" }
        if ([string]$row.Component_ -cne $ComponentId -or
            $row.FileNameIsNull -ne $true -or
            [string]$row.DirProperty -cne $expected[$fileKey] -or
            [int]$row.InstallMode -ne 2) {
            throw "Unsafe RemoveFile row for $fileKey."
        }
        $seen[$fileKey] = $true
    }

    foreach ($fileKey in $expected.Keys) {
        if (-not $seen.ContainsKey($fileKey)) { throw "Missing owned empty-directory removal row: $fileKey" }
    }
}

function Read-MsiRows([string]$Query, [string[]]$Columns, [int[]]$IntegerColumns = @(), [int[]]$NullColumns = @()) {
    $rows = New-Object 'System.Collections.Generic.List[object]'
    $view = $database.OpenView($Query)
    try {
        $view.Execute()
        while ($null -ne ($record = $view.Fetch())) {
            $row = @{}
            for ($index = 0; $index -lt $Columns.Length; $index++) {
                $ordinal = $index + 1
                if ($NullColumns -contains $ordinal) { $row["$($Columns[$index])IsNull"] = [bool]$record.IsNull($ordinal) }
                if ($IntegerColumns -contains $ordinal) {
                    $row[$Columns[$index]] = [int]$record.IntegerData($ordinal)
                } else {
                    $row[$Columns[$index]] = [string]$record.StringData($ordinal)
                }
            }
            $rows.Add($row)
        }
    } finally {
        $view.Close()
    }
    return $rows.ToArray()
}

function Normalize-Guid([string]$Value, [string]$Label) {
    $parsed = [guid]::Empty
    if (-not [guid]::TryParse($Value, [ref]$parsed)) {
        throw "MSI $Label is not a GUID: $Value"
    }
    return $parsed.ToString('D').ToUpperInvariant()
}

$fullPath = [System.IO.Path]::GetFullPath($MsiPath)
if ([System.IO.Path]::GetExtension($fullPath) -ine '.msi' -or
    -not (Test-Path -LiteralPath $fullPath -PathType Leaf) -or
    (Get-Item -LiteralPath $fullPath).Length -le 0) {
    throw "MSI must be an existing non-empty .msi file: $fullPath"
}

$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($fullPath, 0)
$properties = @{}
foreach ($row in @(Read-MsiRows 'SELECT Property, Value FROM Property' @('Property', 'Value'))) {
    $properties[$row.Property] = $row.Value
}
foreach ($property in @('ProductName', 'Manufacturer', 'ProductVersion', 'UpgradeCode')) {
    if (-not $properties.ContainsKey($property)) {
        throw "MSI is missing required property $property."
    }
}
if ($properties.ProductName -cne 'phasegent CLI' -or $properties.Manufacturer -cne 'phasegent') {
    throw 'MSI product name or manufacturer does not match the CLI package.'
}
if ($properties.ProductVersion -cne $Version) {
    throw "MSI ProductVersion $($properties.ProductVersion) does not match $Version."
}
if ((Normalize-Guid $properties.UpgradeCode 'UpgradeCode') -cne $expectedUpgradeCode) {
    throw 'MSI UpgradeCode does not match the stable CLI-only upgrade family.'
}
if ($properties.ContainsKey('ALLUSERS') -and $properties.ALLUSERS -eq '1') {
    throw 'MSI must not select a per-machine installation.'
}
if ($properties.ContainsKey('MSIINSTALLPERUSER')) {
    throw 'Per-user MSI must not expose a machine-selectable install scope.'
}

$summary = $database.SummaryInformation(0)
$template = [string]$summary.Property(7)
$wordCount = [int]$summary.Property(15)
if (($template -split ';')[0] -ine 'x64') {
    throw "MSI summary platform is not x64: $template"
}
if (($wordCount -band 8) -eq 0) {
    throw "MSI summary does not mark a per-user package (WordCount=$wordCount)."
}

$components = @(Read-MsiRows 'SELECT Component, ComponentId, Directory_, Attributes, KeyPath FROM Component' @('Component', 'ComponentId', 'Directory_', 'Attributes', 'KeyPath') @(4))
if ($components.Count -ne 1 -or $components[0].Component -cne $pathComponent) {
    throw "MSI must contain exactly the single owned CLI component; found $($components.Count)."
}
$component = $components[0]
if ((Normalize-Guid $component.ComponentId 'component code') -cne $expectedComponentCode) {
    throw 'MSI component code does not match the stable CLI component identity.'
}
if ($component.Directory_ -cne 'INSTALLFOLDER' -or ($component.Attributes -band 256) -eq 0) {
    throw 'CLI component must be 64-bit and installed in INSTALLFOLDER.'
}
$directories = @(Read-MsiRows 'SELECT Directory, Directory_Parent, DefaultDir FROM Directory' @('Directory', 'Directory_Parent', 'DefaultDir'))
$programs = @($directories | Where-Object { $_.Directory -ceq 'ProgramsFolder' })
$installFolder = @($directories | Where-Object { $_.Directory -ceq 'INSTALLFOLDER' })
if ($programs.Count -ne 1 -or $programs[0].Directory_Parent -cne 'LocalAppDataFolder' -or
    ([string]$programs[0].DefaultDir).Split('|')[-1] -cne 'Programs' -or
    $installFolder.Count -ne 1 -or $installFolder[0].Directory_Parent -cne 'ProgramsFolder' -or
    ([string]$installFolder[0].DefaultDir).Split('|')[-1] -cne 'phasegent-cli') {
    throw 'MSI must install beneath LocalAppDataFolder\Programs\phasegent-cli.'
}

$files = @(Read-MsiRows 'SELECT File, Component_, FileName FROM File' @('File', 'Component_', 'FileName'))
if ($files.Count -ne 1 -or $files[0].Component_ -cne $pathComponent) {
    throw "MSI must contain only the CLI executable payload; found $($files.Count) file rows."
}
$fileNames = ([string]$files[0].FileName).Split('|')
if ($fileNames[-1] -ine 'phasegent.exe') {
    throw "Unexpected MSI payload file: $($files[0].FileName)"
}

$registryRows = @(Read-MsiRows 'SELECT Registry, Root, Key, Name, Value, Component_ FROM Registry' @('Registry', 'Root', 'Key', 'Name', 'Value', 'Component_') @(2))
if ($registryRows.Count -ne 1) {
    throw "MSI must author only its install-path registry value; found $($registryRows.Count) rows."
}
$registry = $registryRows[0]
if ($registry.Root -ne 1 -or $registry.Key -cne 'Software\phasegent\phasegent-cli' -or
    $registry.Name -cne 'InstallPath' -or $registry.Value -cne '[INSTALLFOLDER]' -or
    $registry.Component_ -cne $pathComponent -or $component.KeyPath -cne $registry.Registry) {
    throw 'MSI registry ownership must be one HKCU install-path keypath in the CLI component.'
}

$environmentRows = @(Read-MsiRows 'SELECT Environment, Name, Value, Component_ FROM Environment' @('Environment', 'Name', 'Value', 'Component_'))
if ($environmentRows.Count -ne 1) {
    throw "MSI must author only its user PATH entry; found $($environmentRows.Count) rows."
}
$environment = $environmentRows[0]
if ($environment.Environment -cne 'PhasegentUserPath' -or $environment.Name -cne '=-PATH' -or
    $environment.Value -cne '[~];[INSTALLFOLDER]' -or $environment.Component_ -cne $pathComponent) {
    throw 'MSI PATH entry must append and remove only the owned user install directory.'
}
$removeFileRows = @(Read-MsiRows 'SELECT FileKey, Component_, FileName, DirProperty, InstallMode FROM RemoveFile' @('FileKey', 'Component_', 'FileName', 'DirProperty', 'InstallMode') @(5) @(3))
Assert-PhasegentRemoveFileRows -Rows $removeFileRows -ComponentId $pathComponent

$upgrades = @(Read-MsiRows 'SELECT UpgradeCode, VersionMin, VersionMax, Attributes, ActionProperty FROM Upgrade' @('UpgradeCode', 'VersionMin', 'VersionMax', 'Attributes', 'ActionProperty') @(4))
if ($upgrades.Count -ne 2) {
    throw "MSI MajorUpgrade must author upgrade and downgrade-detection rows; found $($upgrades.Count)."
}
$upgradeCodeRows = @($upgrades | Where-Object { (Normalize-Guid $_.UpgradeCode 'upgrade table code') -ceq $expectedUpgradeCode })
$upgradeRow = @($upgradeCodeRows | Where-Object { $_.ActionProperty -ceq 'WIX_UPGRADE_DETECTED' })
$downgradeRow = @($upgradeCodeRows | Where-Object { $_.ActionProperty -ceq 'WIX_DOWNGRADE_DETECTED' })
if ($upgradeCodeRows.Count -ne 2 -or $upgradeRow.Count -ne 1 -or $downgradeRow.Count -ne 1 -or
    $upgradeRow[0].VersionMax -cne $Version -or ($upgradeRow[0].Attributes -band 2) -ne 0 -or
    $downgradeRow[0].VersionMin -cne $Version -or $downgradeRow[0].VersionMax -ne '' -or
    ($downgradeRow[0].Attributes -band 2) -eq 0) {
    throw 'MSI Upgrade table does not encode forward upgrades and downgrade detection for this version.'
}
$launchConditions = @(Read-MsiRows 'SELECT Condition, Description FROM LaunchCondition' @('Condition', 'Description'))
if ($launchConditions.Count -ne 1 -or $launchConditions[0].Condition -cne 'NOT WIX_DOWNGRADE_DETECTED') {
    throw 'MSI must block installation when a newer product from this family is present.'
}

$tables = @(Read-MsiRows 'SELECT Name FROM _Tables' @('Name') | ForEach-Object { $_.Name })
foreach ($forbiddenTable in @('Shortcut', 'CustomAction', 'RemoveRegistry')) {
    if ($tables -contains $forbiddenTable) {
        throw "MSI must not contain a $forbiddenTable table."
    }
}
Write-Output "MSI tables validated: $fullPath"
