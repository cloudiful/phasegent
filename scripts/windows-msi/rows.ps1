function Read-MsiRows {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Query,
        [Parameter(Mandatory = $true)]
        [string[]]$Columns,
        [int[]]$IntegerColumns = @(),
        [int[]]$NullColumns = @(),
        [Parameter(Mandatory = $true)]
        [object]$Database
    )

    $rows = New-Object 'System.Collections.Generic.List[object]'
    $view = $null
    $failure = $null
    try {
        $view = $Database.OpenView($Query)
        if ($null -eq $view) { throw 'The MSI database returned no view.' }
        [void]$view.Execute()
        while ($null -ne ($record = $view.Fetch())) {
            $properties = [ordered]@{}
            for ($index = 0; $index -lt $Columns.Length; $index++) {
                $ordinal = $index + 1
                if ($NullColumns -contains $ordinal) { $properties["$($Columns[$index])IsNull"] = [bool]$record.IsNull($ordinal) }
                if ($IntegerColumns -contains $ordinal) {
                    $properties[$Columns[$index]] = [int]$record.IntegerData($ordinal)
                } else {
                    $properties[$Columns[$index]] = [string]$record.StringData($ordinal)
                }
            }
            $rows.Add([pscustomobject]$properties)
        }
    } catch {
        $failure = $_.Exception
    }

    if ($null -ne $view) {
        try {
            [void]$view.Close()
        } catch {
            if ($null -eq $failure) {
                $failure = $_.Exception
            } else {
                $failure = [System.InvalidOperationException]::new(
                    "$($failure.Message); closing the view also failed: $($_.Exception.Message)",
                    $failure
                )
            }
        }
    }

    if ($null -ne $failure) {
        throw [System.InvalidOperationException]::new(
            "Failed to read MSI rows using query [$Query]: $($failure.Message)",
            $failure
        )
    }

    foreach ($row in $rows) { Write-Output $row }
}
