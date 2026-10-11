[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ExecutablePath,
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($Tag -cnotmatch '^v(?<version>\d+\.\d+\.\d+)$') {
    throw "Tag must be strict numeric MAJOR.MINOR.PATCH (for example v1.2.3): $Tag"
}
$version = $Matches.version
$parts = $version.Split('.')
$limits = @(255, 255, 65535)
$names = @('MAJOR', 'MINOR', 'PATCH')
for ($index = 0; $index -lt $parts.Length; $index++) {
    $partValue = 0L
    if (-not [long]::TryParse(
        $parts[$index],
        [Globalization.NumberStyles]::None,
        [Globalization.CultureInfo]::InvariantCulture,
        [ref]$partValue
    ) -or $partValue -gt $limits[$index]) {
        throw "MSI $($names[$index]) version must be between 0 and $($limits[$index]): $($parts[$index])"
    }
}

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoPath = Join-Path $root 'Cargo.toml'
$packagePath = Join-Path $root 'package.json'
$cargoText = Get-Content -LiteralPath $cargoPath -Raw
$packageSection = [regex]::Match($cargoText, '(?ms)^\[package\]\s*(?<section>.*?)(?=^\s*\[|\z)')
$cargoVersionMatch = [regex]::Match($packageSection.Groups['section'].Value, '(?m)^\s*version\s*=\s*"(?<version>[^"]+)"')
if (-not $packageSection.Success -or -not $cargoVersionMatch.Success) {
    throw 'Could not read the package version from Cargo.toml.'
}
$packageManifest = Get-Content -LiteralPath $packagePath -Raw | ConvertFrom-Json
$cargoVersion = $cargoVersionMatch.Groups['version'].Value
$packageVersion = [string]$packageManifest.version
if ($cargoVersion -cne $version -or $packageVersion -cne $version) {
    throw "Tag version $version must match Cargo.toml ($cargoVersion) and package.json ($packageVersion)."
}

$source = [System.IO.Path]::GetFullPath($ExecutablePath)
if ([System.IO.Path]::GetExtension($source) -ine '.exe' -or
    -not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Compiled executable must be an existing .exe file: $source"
}
if ((Get-Item -LiteralPath $source).Length -le 0) {
    throw "Compiled executable is empty: $source"
}
$output = [System.IO.Path]::GetFullPath($OutputPath)
if ([System.IO.Path]::GetExtension($output) -ine '.msi') {
    throw "Output path must use the .msi extension: $output"
}
if (Test-Path -LiteralPath $output) {
    throw "Refusing to overwrite an existing MSI: $output"
}

$wix = Get-Command wix -ErrorAction Stop
$wixPath = $wix.Source
$wxsPath = Join-Path $root 'wix/phasegent.wxs'
$outputDirectory = [System.IO.Path]::GetDirectoryName($output)
if (-not (Test-Path -LiteralPath $outputDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

& $wixPath build -arch x64 -d "Version=$version" -d "SourceExe=$source" -o $output $wxsPath
if ($LASTEXITCODE -ne 0) {
    throw "WiX compilation failed with exit code $LASTEXITCODE."
}
if (-not (Test-Path -LiteralPath $output -PathType Leaf) -or
    (Get-Item -LiteralPath $output).Length -le 0) {
    throw "WiX did not produce a non-empty MSI at $output."
}

& (Join-Path $PSScriptRoot 'check-windows-msi.ps1') -MsiPath $output -Version $version
Write-Output "Built and validated $output"
