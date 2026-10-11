Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../../scripts/windows-msi/rows.ps1')

# Contract guard only: this parses the checker's PowerShell AST and mocks
# Database.OpenView with exact expected queries. It is NOT a native MSI SQL
# interpreter, so it proves the literal OpenView contract, not Windows runtime.

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Equal {
    param([object]$Expected, [object]$Actual, [string]$Message)
    if (-not [object]::Equals($Expected, $Actual)) {
        throw "$Message; expected '$Expected', got '$Actual'."
    }
}

# Expected table/column inventory: the only shape a checker query may take.
$expectedQueries = @(
    [pscustomobject]@{ Table = 'Property'; Columns = @('Property', 'Value'); Integers = @(); Nulls = @() }
    [pscustomobject]@{ Table = 'Component'; Columns = @('Component', 'ComponentId', 'Directory_', 'Attributes', 'KeyPath'); Integers = @(4); Nulls = @() }
    [pscustomobject]@{ Table = 'Directory'; Columns = @('Directory', 'Directory_Parent', 'DefaultDir'); Integers = @(); Nulls = @() }
    [pscustomobject]@{ Table = 'File'; Columns = @('File', 'Component_', 'FileName'); Integers = @(); Nulls = @() }
    [pscustomobject]@{ Table = 'Registry'; Columns = @('Registry', 'Root', 'Key', 'Name', 'Value', 'Component_'); Integers = @(2); Nulls = @() }
    [pscustomobject]@{ Table = 'Environment'; Columns = @('Environment', 'Name', 'Value', 'Component_'); Integers = @(); Nulls = @() }
    [pscustomobject]@{ Table = 'RemoveFile'; Columns = @('FileKey', 'Component_', 'FileName', 'DirProperty', 'InstallMode'); Integers = @(5); Nulls = @(3) }
    [pscustomobject]@{ Table = 'Upgrade'; Columns = @('UpgradeCode', 'VersionMin', 'VersionMax', 'Attributes', 'ActionProperty'); Integers = @(4); Nulls = @() }
    [pscustomobject]@{ Table = 'LaunchCondition'; Columns = @('Condition', 'Description'); Integers = @(); Nulls = @() }
    [pscustomobject]@{ Table = '_Tables'; Columns = @('Name'); Integers = @(); Nulls = @() }
)

function Get-ExpectedQuery {
    param([string]$Table)
    $match = @($expectedQueries | Where-Object { $_.Table -ceq $Table })
    if ($match.Count -ne 1) { return $null }
    return $match[0]
}

# Every table and column name must be individually wrapped in grave accents
# (ASCII 96) per the official MSI SQL syntax; brackets and SQL quotes are not
# escaping, and identifier names stay case-sensitive and exact.
function Get-MsiQueryIdentifiers {
    param([string]$Query)
    $shape = [regex]::Match($Query, '^SELECT (?<columns>.+) FROM `(?<table>[^`]+)`$')
    if (-not $shape.Success) { return $null }
    $columns = @($shape.Groups['columns'].Value.Split(', ') | ForEach-Object {
        $name = [regex]::Match($_, '^`(?<name>[^`]+)`$')
        if (-not $name.Success) { return "`$_" }
        return $name.Groups['name'].Value
    })
    return [pscustomobject]@{
        Table   = $shape.Groups['table'].Value
        Columns = $columns
    }
}

function Test-MsiQueryContract {
    param([string]$Query)
    if ($Query.Contains('[') -or $Query.Contains("'")) { return $false }
    $identifiers = Get-MsiQueryIdentifiers -Query $Query
    if ($null -eq $identifiers) { return $false }
    $expected = Get-ExpectedQuery -Table $identifiers.Table
    if ($null -eq $expected) { return $false }
    if ($identifiers.Columns.Count -ne $expected.Columns.Count) { return $false }
    for ($index = 0; $index -lt $expected.Columns.Count; $index++) {
        if ($identifiers.Columns[$index] -cne $expected.Columns[$index]) { return $false }
    }
    return $true
}

function Get-CheckerQueries {
    param([string]$CheckerPath)
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $CheckerPath, [ref]$tokens, [ref]$errors)
    if ($errors.Count -ne 0) { throw "The MSI checker does not parse: $($errors[0].Message)" }
    return @($ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -ceq 'Read-MsiRows'
    }, $true))
}

# Positional and named arguments of a Read-MsiRows call, read from the AST so
# the assertions cover the literal values the reader is actually handed.
function Get-ReaderArguments {
    param([object]$Call)
    $positional = @()
    $named = @{}
    $elements = @($Call.CommandElements)
    for ($index = 1; $index -lt $elements.Count; $index++) {
        $element = $elements[$index]
        if ($element -is [System.Management.Automation.Language.CommandParameterAst]) {
            $value = $null
            if ($index + 1 -lt $elements.Count) { $value = $elements[$index + 1] }
            $named[$element.ParameterName] = $value
            $index++
            continue
        }
        $positional += $element
    }
    return [pscustomobject]@{
        Query    = [string]$positional[0].Value
        Columns  = @(Convert-AstArray -Element $positional[1])
        Integers = if ($positional.Count -gt 2) { @(Convert-AstArray -Element $positional[2]) } else { @() }
        Nulls    = if ($positional.Count -gt 3) { @(Convert-AstArray -Element $positional[3]) } else { @() }
        Named    = $named
    }
}

# `@(1, 2)` parses as an ArrayLiteralAst and single-element `@(2)` collapses to
# one constant expression; both keep the literals the reader receives.
function Convert-AstArray {
    param([object]$Element)
    if ($null -eq $Element) { return @() }
    Assert-True ($Element -is [System.Management.Automation.Language.ArrayExpressionAst]) `
        'checker arguments must be literal array expressions'
    $literal = $Element.SubExpression.Statements[0].PipelineElements[0].Expression
    Assert-True ($literal -is [System.Management.Automation.Language.ArrayLiteralAst] -or
        $literal -is [System.Management.Automation.Language.ConstantExpressionAst]) `
        'checker array arguments must contain only literals'
    $elements = if ($literal -is [System.Management.Automation.Language.ArrayLiteralAst]) {
        @($literal.Elements)
    } else {
        @($literal)
    }
    return @($elements | ForEach-Object {
        Assert-True ($_ -is [System.Management.Automation.Language.ConstantExpressionAst]) `
            'checker array elements must be literals'
        $_.Value
    })
}

# The evaluated StringConstantExpressionAst value is what the reader receives,
# so the assertions below run against runtime strings, not source fragments.
$checker = Join-Path $PSScriptRoot '../../scripts/check-windows-msi.ps1'
$calls = @(Get-CheckerQueries -CheckerPath $checker)
Assert-Equal 10 $calls.Count 'checker query count'
Assert-Equal $expectedQueries.Count $calls.Count 'one query per expected MSI table'

$actualQueries = @()
foreach ($call in $calls) {
    Assert-True ($call.CommandElements[1] -is [System.Management.Automation.Language.StringConstantExpressionAst]) `
        'every checker query must be a literal string constant'
    $arguments = Get-ReaderArguments -Call $call
    $query = $arguments.Query
    $actualQueries += $query

    Assert-True (Test-MsiQueryContract -Query $query) "query is not escaped per MSI SQL syntax: $query"
    $identifiers = Get-MsiQueryIdentifiers -Query $query
    $expected = Get-ExpectedQuery -Table $identifiers.Table

    Assert-Equal $expected.Columns.Count $arguments.Columns.Count "column mapping width for $query"
    for ($index = 0; $index -lt $expected.Columns.Count; $index++) {
        Assert-Equal $expected.Columns[$index] $arguments.Columns[$index] "column mapping order for $query"
    }

    # Typed-null and integer ordinal mappings are safety checks, so quoting the
    # identifiers must not shift or drop them.
    Assert-Equal (@($expected.Integers) -join ',') (@($arguments.Integers) -join ',') "integer columns for $query"
    Assert-Equal (@($expected.Nulls) -join ',') (@($arguments.Nulls) -join ',') "null columns for $query"
    Assert-True ($arguments.Named.ContainsKey('Database')) "every query passes its opened database: $query"
}

# Runtime grave accents must survive into the string handed to OpenView,
# including the reserved-word collisions that broke the native checker.
$registryQuery = @($actualQueries | Where-Object { $_.Contains('FROM `Registry`') })
Assert-Equal 1 $registryQuery.Count 'Registry query count'
$expectedRegistry = 'SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`'
Assert-Equal $expectedRegistry $registryQuery[0] 'Registry OpenView query'
foreach ($reserved in @('Registry', 'Key', 'Value')) {
    Assert-True ($registryQuery[0].Contains("``$reserved``")) "runtime query must keep ASCII 96 around $reserved"
}
Assert-Equal $expectedQueries.Count (@($actualQueries | Sort-Object -Unique).Count) 'query uniqueness'
Assert-True (@($actualQueries | Where-Object { $_.Contains('``') }).Count -eq 0) 'no doubled grave accents'
Assert-True (@($actualQueries | Where-Object { $_.Contains('`` ') }).Count -eq 0) 'no stray grave accents'

# The original unquoted query, and near misses, must fail the contract; the
# test never widens to accept an arbitrary generic query.
$rejected = @(
    'SELECT Registry, Root, Key, Name, Value, Component_ FROM Registry',
    'SELECT `Registry`, `Value`, `Component_`, `Name`, `Key`, `Root` FROM `Registry`',
    'SELECT `Registry`, `Root`, `key`, `Name`, `Value`, `Component_` FROM `Registry`',
    'SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`, `File`',
    'SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_` FROM [Registry]',
    'SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_`'
)
foreach ($query in $rejected) {
    Assert-True (-not (Test-MsiQueryContract -Query $query)) "contract must reject: $query"
}
Assert-True (-not (Test-MsiQueryContract -Query 'SELECT `Name` FROM `Shortcut`')) `
    'contract must reject an unowned table'

# Mock expectation guard: the fake accepts only the ten exact queries and
# serves per-query rows, so a mismatched or generic query fails instead of
# silently reading rows.
function New-FakeRecord {
    param([object[]]$Values, [bool[]]$Nulls)
    $record = [pscustomobject]@{ Values = $Values; Nulls = $Nulls }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name StringData -Value {
        param([int]$Ordinal)
        return [string]$this.Values[$Ordinal - 1]
    }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name IntegerData -Value {
        param([int]$Ordinal)
        return [int]$this.Values[$Ordinal - 1]
    }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name IsNull -Value {
        param([int]$Ordinal)
        return [bool]$this.Nulls[$Ordinal - 1]
    }
    return $record
}

# The fake accepts only the exact expected queries and serves per-query rows, so
# a mismatched or generic query fails instead of silently reading rows. A
# missing row key yields the empty result set.
function New-FakeDatabase {
    param([string[]]$Accepted, [hashtable]$RowsByQuery)
    $database = [pscustomobject]@{ Accepted = $Accepted; RowsByQuery = $RowsByQuery; Queries = @() }
    Add-Member -InputObject $database -MemberType ScriptMethod -Name OpenView -Value {
        param([string]$Query)
        $this.Queries += $Query
        if ($this.Accepted -cnotcontains $Query) { throw "unexpected OpenView query: $Query" }
        $queue = [System.Collections.Queue]::new()
        foreach ($row in @($this.RowsByQuery[$Query])) { $queue.Enqueue($row) }
        $view = [pscustomobject]@{ Queue = $queue }
        Add-Member -InputObject $view -MemberType ScriptMethod -Name Execute -Value { return 'ExecuteNoise' }
        Add-Member -InputObject $view -MemberType ScriptMethod -Name Fetch -Value {
            if ($this.Queue.Count -eq 0) { return $null }
            return $this.Queue.Dequeue()
        }
        Add-Member -InputObject $view -MemberType ScriptMethod -Name Close -Value { return 'CloseNoise' }
        return $view
    }
    return $database
}

$registryColumns = (Get-ExpectedQuery -Table 'Registry').Columns
$environmentColumns = (Get-ExpectedQuery -Table 'Environment').Columns
$environmentQuery = 'SELECT `Environment`, `Name`, `Value`, `Component_` FROM `Environment`'
$tablesQuery = 'SELECT `Name` FROM `_Tables`'
$rowsByQuery = @{
    $expectedRegistry = @(New-FakeRecord -Values @(
        'Registry', 1, 'Software\phasegent\phasegent-cli', 'InstallPath', '[INSTALLFOLDER]', 'PhasegentCli'
    ) -Nulls @($false, $false, $false, $false, $false, $false))
    $environmentQuery = @(New-FakeRecord -Values @(
        'PhasegentUserPath', '=-PATH', '[~];[INSTALLFOLDER]', 'PhasegentCli'
    ) -Nulls @($false, $false, $false, $false))
}
$database = New-FakeDatabase -Accepted $actualQueries -RowsByQuery $rowsByQuery

$emptyRows = @(Read-MsiRows -Database $database -Query $tablesQuery -Columns @('Name'))
Assert-Equal 0 $emptyRows.Count 'empty query row count'
Assert-Equal $tablesQuery $database.Queries[0] 'OpenView receives the exact escaped empty query'

$rows = @(Read-MsiRows -Database $database -Query $registryQuery[0] -Columns $registryColumns @(2))
Assert-Equal 1 $rows.Count 'Registry row count'
Assert-Equal $expectedRegistry $database.Queries[1] 'OpenView receives the exact escaped query'
Assert-Equal 'Registry,Root,Key,Name,Value,Component_' `
    (@($rows[0].PSObject.Properties | ForEach-Object { $_.Name }) -join ',') 'stable Registry row shape'
Assert-Equal 'Software\phasegent\phasegent-cli' $rows[0].Key 'Registry Key column mapping'
Assert-Equal 'InstallPath' $rows[0].Name 'Registry Name column mapping'
Assert-Equal '[INSTALLFOLDER]' $rows[0].Value 'Registry Value column mapping'
Assert-Equal 'PhasegentCli' $rows[0].Component_ 'Registry Component_ column mapping'
Assert-True ($rows[0].Root -is [int]) 'integer columns stay typed under escaped queries'

# Case-sensitive column mapping is what rejects a wrong-case table contract.
$environmentRows = @(Read-MsiRows -Database $database -Query $environmentQuery -Columns $environmentColumns)
Assert-Equal 1 $environmentRows.Count 'Environment row count'
Assert-Equal $environmentQuery $database.Queries[2] 'Environment OpenView query is escaped'
Assert-Equal 'PhasegentUserPath' $environmentRows[0].Environment 'Environment column mapping'
Assert-Equal '=-PATH' $environmentRows[0].Name 'Environment Name column mapping'

$rejectedDatabase = New-FakeDatabase -Accepted @($expectedRegistry) -RowsByQuery @{}
$rejectedError = $null
try {
    Read-MsiRows -Database $rejectedDatabase -Query $rejected[0] -Columns $registryColumns | Out-Null
} catch {
    $rejectedError = $_
}
Assert-True ($null -ne $rejectedError) 'the original unquoted query must be rejected by the reader contract'
Assert-True ($rejectedError.Exception.Message.Contains("query [$($rejected[0])]")) `
    'rejection keeps the failing query context'
Assert-True ($rejectedError.Exception.InnerException.Message.Contains('unexpected OpenView query')) `
    'rejection surfaces the exact OpenView argument mismatch'

Write-Output "MSI query contract validated: $($calls.Count) escaped OpenView queries"
