Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../../scripts/windows-msi/rows.ps1')

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

function Assert-ContextualFailure {
    param(
        [System.Management.Automation.ErrorRecord]$Failure,
        [string]$Query,
        [string]$Method,
        [string]$Cause,
        [string]$ContextAssertion
    )
    Assert-True ($Failure.Exception -is [System.InvalidOperationException]) 'failure must keep its contextual outer type'
    Assert-True ($Failure.Exception.Message.Contains("query [$Query]")) $ContextAssertion
    $wrapped = $Failure.Exception.InnerException
    Assert-True ($wrapped -is [System.Management.Automation.MethodInvocationException]) 'ScriptMethod failure wrapper must be preserved'
    Assert-True ($wrapped.Message.Contains($Method)) 'wrapped failure must identify the failing method'
    Assert-True ($wrapped.Message.Contains($Cause)) 'wrapped failure must include its original detail'
    $originalDetailFound = $false
    while ($null -ne $wrapped) {
        if ($wrapped.Message -ceq $Cause) { $originalDetailFound = $true }
        $wrapped = $wrapped.InnerException
    }
    Assert-True $originalDetailFound 'original failure detail must remain in the exception chain'
}

function New-FakeRecord {
    param([object[]]$Values, [bool[]]$Nulls)
    $record = [pscustomobject]@{ Values = $Values; Nulls = $Nulls }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name StringData -Value {
        param([int]$Ordinal)
        if ($Ordinal -gt $this.Values.Count) { throw "fake field ordinal $Ordinal is missing" }
        return [string]$this.Values[$Ordinal - 1]
    }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name IntegerData -Value {
        param([int]$Ordinal)
        if ($Ordinal -gt $this.Values.Count) { throw "fake field ordinal $Ordinal is missing" }
        return [int]$this.Values[$Ordinal - 1]
    }
    Add-Member -InputObject $record -MemberType ScriptMethod -Name IsNull -Value {
        param([int]$Ordinal)
        if ($Ordinal -gt $this.Nulls.Count) { throw "fake null ordinal $Ordinal is missing" }
        return [bool]$this.Nulls[$Ordinal - 1]
    }
    return $record
}

function New-FakeView {
    param([object[]]$Rows, [string]$FailOn = '')
    $queue = [System.Collections.Queue]::new()
    foreach ($row in $Rows) { $queue.Enqueue($row) }
    $view = [pscustomobject]@{ Rows = $queue; FailOn = $FailOn; CloseCount = 0 }
    Add-Member -InputObject $view -MemberType ScriptMethod -Name Execute -Value {
        if ($this.FailOn -ceq 'Execute' -or $this.FailOn -ceq 'ExecuteAndClose') { throw 'fake Execute failure' }
        return 'ExecuteNoise'
    }
    Add-Member -InputObject $view -MemberType ScriptMethod -Name Fetch -Value {
        if ($this.FailOn -ceq 'Fetch') { throw 'fake Fetch failure' }
        if ($this.Rows.Count -eq 0) { return $null }
        return $this.Rows.Dequeue()
    }
    Add-Member -InputObject $view -MemberType ScriptMethod -Name Close -Value {
        $this.CloseCount = $this.CloseCount + 1
        if ($this.FailOn -ceq 'Close' -or $this.FailOn -ceq 'ExecuteAndClose') { throw 'fake Close failure' }
        return 'CloseNoise'
    }
    return $view
}

function New-FakeDatabase {
    param([object]$View, [string]$FailOn = '')
    $database = [pscustomobject]@{ View = $View; FailOn = $FailOn }
    Add-Member -InputObject $database -MemberType ScriptMethod -Name OpenView -Value {
        param([string]$Query)
        if ($this.FailOn -ceq 'OpenView') { throw "fake OpenView failure for $Query" }
        return $this.View
    }
    return $database
}

$propertyQuery = 'SELECT `Property`, `Value` FROM `Property`'
$propertyColumns = @('Property', 'Value')
$emptyRows = @(Read-MsiRows -Database (New-FakeDatabase -View (New-FakeView -Rows @())) -Query $propertyQuery -Columns $propertyColumns)
Assert-Equal 0 $emptyRows.Count 'empty query row count'

$propertyRecords = @(
    (New-FakeRecord -Values @('ProductName', 'phasegent CLI') -Nulls @($false, $false)),
    (New-FakeRecord -Values @('Manufacturer', '') -Nulls @($false, $false))
)
$propertyView = New-FakeView -Rows $propertyRecords
$propertyRows = @(Read-MsiRows -Database (New-FakeDatabase -View $propertyView) -Query $propertyQuery -Columns $propertyColumns)
Assert-Equal 2 $propertyRows.Count 'multiple query row count'
Assert-True ($propertyRows[0] -is [pscustomobject]) 'rows must be PSCustomObjects only'
Assert-True ($propertyRows[1] -is [pscustomobject]) 'rows must be PSCustomObjects only'
Assert-Equal 'ProductName' $propertyRows[0].Property 'first property name'
Assert-Equal 'phasegent CLI' $propertyRows[0].Value 'non-empty MSI value'
Assert-Equal 'Manufacturer' $propertyRows[1].Property 'second property name'
Assert-Equal '' $propertyRows[1].Value 'empty MSI value must remain present'
Assert-Equal 'Property,Value' (@($propertyRows[0].PSObject.Properties | ForEach-Object { $_.Name }) -join ',') 'stable row shape'
Assert-Equal 1 $propertyView.CloseCount 'view closes after successful read'

$singleRows = @(Read-MsiRows -Database (New-FakeDatabase -View (New-FakeView -Rows @(
    (New-FakeRecord -Values @('ProductVersion', '2.26.0') -Nulls @($false, $false))
))) -Query $propertyQuery -Columns $propertyColumns)
Assert-Equal 1 $singleRows.Count 'single query row count'
Assert-True ($singleRows[0] -is [pscustomobject]) 'single result must be a PSCustomObject'
Assert-Equal '2.26.0' $singleRows[0].Value 'single result value'

$removeFileQuery = 'SELECT `FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`'
$removeFileColumns = @('FileKey', 'Component_', 'FileName', 'DirProperty', 'InstallMode')
$removeFileRecords = @(
    (New-FakeRecord -Values @('RemoveInstallFolder', 'PhasegentCli', '', 'INSTALLFOLDER', 2) -Nulls @($false, $false, $true, $false, $false)),
    (New-FakeRecord -Values @('RemoveProgramsFolder', 'PhasegentCli', 'KeepName', 'ProgramsFolder', 2) -Nulls @($false, $false, $false, $false, $false))
)
$removeFileRows = @(Read-MsiRows -Database (New-FakeDatabase -View (New-FakeView -Rows $removeFileRecords)) -Query $removeFileQuery -Columns $removeFileColumns -IntegerColumns @(5) -NullColumns @(3))
Assert-Equal 2 $removeFileRows.Count 'typed query row count'
Assert-True ($removeFileRows[0].InstallMode -is [int]) 'integer columns remain typed'
Assert-True ($removeFileRows[0].FileNameIsNull -is [bool]) 'null flags remain typed'
Assert-Equal $true $removeFileRows[0].FileNameIsNull 'nullable MSI field flag'
Assert-Equal $false $removeFileRows[1].FileNameIsNull 'non-null MSI field flag'
Assert-Equal '' $removeFileRows[0].FileName 'null string field retains an empty Value'

$executeView = New-FakeView -Rows @() -FailOn 'Execute'
$executeError = $null
$executeRows = @()
try {
    $executeRows = @(Read-MsiRows -Database (New-FakeDatabase -View $executeView) -Query $propertyQuery -Columns $propertyColumns)
} catch {
    $executeError = $_
}
Assert-True ($null -ne $executeError) 'Execute errors must propagate'
Assert-ContextualFailure $executeError $propertyQuery 'Execute' 'fake Execute failure' 'Execute errors include query context'
Assert-Equal 0 $executeRows.Count 'Execute failure emits no rows'
Assert-Equal 1 $executeView.CloseCount 'view closes after Execute failure'

$fetchView = New-FakeView -Rows @((New-FakeRecord -Values @('ProductName', 'phasegent CLI') -Nulls @($false, $false))) -FailOn 'Fetch'
$fetchError = $null
$fetchRows = @()
try {
    $fetchRows = @(Read-MsiRows -Database (New-FakeDatabase -View $fetchView) -Query $propertyQuery -Columns $propertyColumns)
} catch {
    $fetchError = $_
}
Assert-True ($null -ne $fetchError) 'Fetch errors must propagate'
Assert-ContextualFailure $fetchError $propertyQuery 'Fetch' 'fake Fetch failure' 'Fetch errors include query context'
Assert-Equal 0 $fetchRows.Count 'Fetch failure emits no rows'

$columnQuery = 'SELECT `Property`, `Value`, `MissingColumn` FROM `Property`'
$columnView = New-FakeView -Rows @(
    (New-FakeRecord -Values @('ProductName', 'phasegent CLI', 'Ready') -Nulls @($false, $false, $false)),
    (New-FakeRecord -Values @('Manufacturer', '') -Nulls @($false, $false))
)
$columnError = $null
$columnRows = @()
try {
    $columnRows = @(Read-MsiRows -Database (New-FakeDatabase -View $columnView) -Query $columnQuery -Columns @('Property', 'Value', 'MissingColumn'))
} catch {
    $columnError = $_
}
Assert-True ($null -ne $columnError) 'missing row fields must propagate'
Assert-ContextualFailure $columnError $columnQuery 'StringData' 'fake field ordinal 3 is missing' 'row-field errors include query context'
Assert-Equal 0 $columnRows.Count 'row-field failure emits no partial rows'

$closeView = New-FakeView -Rows @((New-FakeRecord -Values @('ProductName', 'phasegent CLI') -Nulls @($false, $false))) -FailOn 'Close'
$closeError = $null
$closeRows = @()
try {
    $closeRows = @(Read-MsiRows -Database (New-FakeDatabase -View $closeView) -Query $propertyQuery -Columns $propertyColumns)
} catch {
    $closeError = $_
}
Assert-True ($null -ne $closeError) 'Close errors must propagate'
Assert-ContextualFailure $closeError $propertyQuery 'Close' 'fake Close failure' 'Close errors include query context'
Assert-Equal 0 $closeRows.Count 'Close failure emits no buffered rows'
Assert-Equal 1 $closeView.CloseCount 'failed Close is attempted once'

$dualView = New-FakeView -Rows @((New-FakeRecord -Values @('ProductName', 'phasegent CLI') -Nulls @($false, $false))) -FailOn 'ExecuteAndClose'
$dualError = $null
$dualRows = @()
try {
    $dualRows = @(Read-MsiRows -Database (New-FakeDatabase -View $dualView) -Query $propertyQuery -Columns $propertyColumns)
} catch {
    $dualError = $_
}
Assert-True ($null -ne $dualError) 'Execute plus Close failures must propagate'
Assert-True ($dualError.Exception -is [System.InvalidOperationException]) 'dual failure keeps contextual outer type'
Assert-True ($dualError.Exception.Message.Contains("query [$propertyQuery]")) 'dual failure keeps query context'
$combinedFailure = $dualError.Exception.InnerException
Assert-True ($combinedFailure -is [System.InvalidOperationException]) 'dual failure preserves both causes in its inner type'
Assert-True ($combinedFailure.Message.Contains('fake Execute failure')) 'dual failure retains Execute detail'
Assert-True ($combinedFailure.Message.Contains('fake Close failure')) 'dual failure retains Close detail'
Assert-True ($combinedFailure.Message.Contains('closing the view also failed')) 'dual failure identifies Close context'
Assert-True ($combinedFailure.InnerException -is [System.Management.Automation.MethodInvocationException]) 'dual failure retains wrapped Execute cause'
Assert-Equal 0 $dualRows.Count 'dual failure emits no buffered rows'
Assert-Equal 1 $dualView.CloseCount 'dual failure attempts Close once'
