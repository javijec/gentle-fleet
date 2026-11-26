# Native lifecycle regressions; no external provider or user data.
param(
    [ValidateSet('All', 'Lifecycle', 'MissingRunner', 'InvalidRunner', 'SilentRunner', 'Interrupt', 'InterruptFailure', 'ActiveStop', 'CloseFailure', 'CloseBlocked')]
    [string]$Scenario = 'All'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($Scenario -eq 'All') {
    foreach ($case in @('Lifecycle', 'MissingRunner', 'InvalidRunner', 'SilentRunner', 'Interrupt', 'InterruptFailure', 'ActiveStop', 'CloseFailure', 'CloseBlocked')) {
        Write-Output "W1-D scenario: $case"
        & $PSCommandPath -Scenario $case
    }
    return
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet w1d " + [guid]::NewGuid().ToString('N'))
$bin = Join-Path $root 'isolated binaries'
$runtime = Join-Path $root 'runtime'
$workspace = Join-Path $root 'workspace with spaces'
$runnerExe = Join-Path $bin 'fleet-runner.exe'
$daemon = $null
$sentinel = $null
$runners = @{}
$script:fixtureOperationKey = $null
# Inherit no fake fault flags or prior storage/catalog overrides.
$names = @('FLEET_RUNTIME_DIR', 'FLEET_DB_PATH', 'FLEET_RUNNER_BIN',
    'FLEET_ADOPT_LEGACY_DB', 'FLEET_OPERATION_KEY', 'FLEET_OPERATION_JOURNAL',
    'FLEET_CURSOR_JOURNAL', 'FLEET_SSH_CONFIG', 'FLEET_HOST_ID')
$names += @(Get-ChildItem Env: | Where-Object { $_.Name -like 'FLEET_FAKE_*' } |
    ForEach-Object { $_.Name })

function Start-OwnedProcess([string]$Executable, [string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    # Remove inherited entries from the child environment dictionary itself.
    # Assigning a PowerShell $null through a .NET string parameter can leave
    # an empty environment value instead of an absent variable.
    foreach ($name in $names) {
        [void]$info.Environment.Remove($name)
    }
    $info.Environment['FLEET_RUNTIME_DIR'] = $runtime
    $info.Environment['FLEET_DB_PATH'] = Join-Path $root 'private data/fleet.sqlite'
    if ($null -ne $script:fixtureOperationKey) {
        $info.Environment['FLEET_OPERATION_KEY'] = $script:fixtureOperationKey
    }
    if ($info.Environment.ContainsKey('FLEET_RUNNER_BIN')) {
        throw 'fixture must not pass a runner binary override'
    }
    if ($Executable -eq (Join-Path $bin 'fleetd.exe')) {
        if ($Scenario -in @('Interrupt', 'InterruptFailure', 'ActiveStop')) {
            $info.Environment['FLEET_FAKE_DEFER_TERMINAL'] = '1'
        }
        if ($Scenario -eq 'InterruptFailure') {
            $info.Environment['FLEET_FAKE_FAIL_INTERRUPT'] = '1'
        }
        if ($Scenario -eq 'CloseFailure') {
            $info.Environment['FLEET_FAKE_FAIL_CLOSE'] = '1'
        }
        if ($Scenario -eq 'CloseBlocked') {
            $info.Environment['FLEET_FAKE_BLOCK_CLOSE'] = '1'
        }
    }
    foreach ($argument in $Arguments) { [void]$info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    $process | Add-Member -NotePropertyName FixtureStdout -NotePropertyValue ($process.StandardOutput.ReadToEndAsync())
    $process | Add-Member -NotePropertyName FixtureStderr -NotePropertyValue ($process.StandardError.ReadToEndAsync())
    return $process
}

function Stop-OwnedProcess($Process) {
    if ($null -eq $Process) { return }
    try {
        if (-not $Process.HasExited) {
            $Process.Kill()
            if (-not $Process.WaitForExit(5000)) { throw 'owned fixture process did not exit' }
        }
    } finally {
        $Process.Dispose()
    }
}

function Invoke-FixtureCli([string[]]$Arguments, [string]$ExpectedError = '') {
    $process = Start-OwnedProcess (Join-Path $bin 'fleetctl.exe') $Arguments
    try {
        $timeout = if ($Scenario -eq 'SilentRunner' -and $Arguments[0] -eq 'session' -and
            $Arguments[1] -eq 'create') { 90000 } else { 20000 }
        if (-not $process.WaitForExit($timeout)) { throw "CLI timed out: $($Arguments[0])" }
        if ($process.ExitCode -ne 0) {
            $err = if ($process.FixtureStderr.IsCompletedSuccessfully) {
                $process.FixtureStderr.GetAwaiter().GetResult()
            } else { '<stderr unavailable>' }
            if ($ExpectedError -and $err -match $ExpectedError) { return $err }
            throw "CLI $($Arguments[0]) exit $($process.ExitCode): $err"
        }
        if ($ExpectedError) { throw "CLI unexpectedly succeeded: $($Arguments[0])" }
        if (-not $process.FixtureStdout.Wait(5000)) { throw 'CLI stdout did not close' }
        return $process.FixtureStdout.GetAwaiter().GetResult().Trim()
    } finally {
        Stop-OwnedProcess $process
    }
}

function Track-RunnerChildren {
    if ($null -eq $daemon -or $daemon.HasExited) { return }
    # Match only children of this live daemon with the exact executable copied
    # to this unique fixture directory. Retain the OS process handle.
    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($daemon.Id)" |
        Where-Object { $_.ExecutablePath -eq $runnerExe })
    foreach ($child in $children) {
        $id = [int]$child.ProcessId
        if ($runners.ContainsKey($id)) { continue }
        $process = Get-Process -Id $id -ErrorAction Stop
        try {
            [void]$process.Handle
            if ($process.HasExited -or $process.Path -ne $runnerExe) {
                throw 'runner identity changed while capturing process handle'
            }
            $current = @(Get-CimInstance Win32_Process -Filter "ProcessId=$id")
            if ($current.Count -ne 1 -or $current[0].ParentProcessId -ne $daemon.Id -or
                $current[0].ExecutablePath -ne $runnerExe -or
                $current[0].CreationDate -ne $child.CreationDate) {
                throw 'runner identity changed while capturing process handle'
            }
            $runners[$id] = $process
            $process = $null
        } finally {
            if ($null -ne $process) { $process.Dispose() }
        }
    }
}

function New-FixtureSession([string]$WorkspaceId) {
    $before = @($runners.Keys)
    try {
        $receipt = Invoke-FixtureCli @('session', 'create', '--workspace', $WorkspaceId, '--provider', 'fake')
    } finally {
        Track-RunnerChildren
    }
    $fields = $receipt -split "\t"
    if ($fields.Count -ne 4 -or $fields[1] -ne 'fake' -or $fields[2] -ne 'Running') {
        throw 'unexpected session creation receipt'
    }
    $new = @($runners.Keys | Where-Object { $_ -notin $before })
    if ($new.Count -ne 1) { throw 'expected exactly one new owned runner' }
    return @{ Session = $fields[0]; Process = $runners[$new[0]] }
}

function Assert-CompletedTurn([string]$Session, [string]$Message) {
    $key = [guid]::NewGuid().ToString('N')
    $script:fixtureOperationKey = $key
    $receipt = (Invoke-FixtureCli @('prompt', $Session, $Message)) -split "\t"
    if ($receipt.Count -ne 3 -or $receipt[1] -ne $Session -or -not $receipt[2]) {
        throw 'prompt receipt does not identify the expected session/turn'
    }
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    do {
        $timeline = Invoke-FixtureCli @('timeline', $Session, '--since', '0', '--limit', '256')
        $operation = (Invoke-FixtureCli @('operation', $key)) -split "\t"
        $messages = @($timeline -split "\r?\n" | Where-Object { $_ -match "\tuser\t$([regex]::Escape($Message))$" })
        if ($operation.Count -ge 4 -and $operation[0] -eq $receipt[0] -and
            $operation[1] -eq $key -and $operation[3] -eq 'Completed' -and
            $messages.Count -eq 1 -and $timeline -match '\tdelta\t' -and
            $timeline -match '\tturn_completed\tCompleted') { return }
        Start-Sleep -Milliseconds 100
    } while ($deadline.Elapsed.TotalSeconds -lt 20)
    throw 'expected prompt, streamed delta and durable completed operation were not observed'
}

function Assert-Stopped($Fixture) {
    $reply = Invoke-FixtureCli @('stop', $Fixture.Session)
    if ($reply -ne ("stopped" + [char]9 + $Fixture.Session)) {
        throw 'stop did not return the expected session'
    }
    if (-not $Fixture.Process.WaitForExit(3000) -or $Fixture.Process.ExitCode -ne 0) {
        throw 'confirmed stop did not correspond to successful runner exit'
    }
    $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
        Where-Object { ($_ -split "\t")[0] -eq $Fixture.Session })
    if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'Stopped') {
        throw 'session list did not confirm Stopped'
    }
    $timeline = Invoke-FixtureCli @('timeline', $Fixture.Session, '--since', '0', '--limit', '256')
    if ($timeline -notmatch '\tsession_state\tStopped') { throw 'durable Stopped event missing' }
}

function Start-DeferredTurn([string]$Session) {
    $script:fixtureOperationKey = [guid]::NewGuid().ToString('N')
    $receipt = (Invoke-FixtureCli @('prompt', $Session, 'W1-D deferred turn')) -split "\t"
    if ($receipt.Count -ne 3 -or $receipt[1] -ne $Session -or -not $receipt[2]) {
        throw 'deferred prompt receipt is invalid'
    }
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    do {
        $timeline = Invoke-FixtureCli @('timeline', $Session, '--since', '0', '--limit', '256')
        if ($timeline -match '\tturn_completed\t') { throw 'deferred turn completed before interrupt or stop' }
        if ($timeline -match '\tdelta\t') { return $receipt[2] }
        Start-Sleep -Milliseconds 100
    } while ($deadline.Elapsed.TotalSeconds -lt 20)
    throw 'deferred turn did not produce a delta'
}

function Assert-UnconfirmedStop($Fixture) {
    $null = Invoke-FixtureCli @('stop', $Fixture.Session) 'StopUnconfirmed'
    if ($Fixture.Process.HasExited) { throw 'unconfirmed close unexpectedly killed the runner' }
    $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
        Where-Object { ($_ -split "\t")[0] -eq $Fixture.Session })
    if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'StopUnconfirmed') {
        throw 'unconfirmed stop state missing'
    }
    $timeline = Invoke-FixtureCli @('timeline', $Fixture.Session, '--since', '0', '--limit', '256')
    if ($timeline -notmatch '\tsession_state\tStopUnconfirmed' -or $timeline -match '\tsession_state\tStopped') {
        throw 'unconfirmed stop produced an invalid durable state'
    }
}

New-Item -ItemType Directory -Path $root | Out-Null
try {
    New-Item -ItemType Directory -Path $bin, $workspace | Out-Null
    foreach ($exe in @('fleetd.exe', 'fleet-runner.exe', 'fleetctl.exe')) {
        if ($exe -eq 'fleet-runner.exe' -and $Scenario -in @('MissingRunner', 'InvalidRunner', 'SilentRunner')) {
            continue
        }
        Copy-Item -LiteralPath (Join-Path $repo "target/debug/$exe") -Destination $bin
    }
    if ($Scenario -eq 'InvalidRunner') {
        # An owned executable exits with CLI usage, without announcing runner identity.
        Copy-Item -LiteralPath (Join-Path $bin 'fleetctl.exe') -Destination $runnerExe
    }
    if ($Scenario -eq 'SilentRunner') {
        # Native fixture does not spawn descendants or write to stdout.
        & rustc --edition=2021 --crate-name w1d_silent_runner (Join-Path $PSScriptRoot 'fixtures/w1d-silent-runner.rs') -o $runnerExe
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $runnerExe)) {
            throw 'silent runner fixture did not compile'
        }
    }
    # No FLEET_RUNNER_BIN: exercise default sibling executable resolution.
    $sentinel = Start-OwnedProcess (Join-Path $PSHOME 'pwsh.exe') @('-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 180')
    $daemon = Start-OwnedProcess (Join-Path $bin 'fleetd.exe') @()
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath (Join-Path $runtime 'fleetd.addr'))) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'daemon failed to publish fixture endpoint'
        }
        Start-Sleep -Milliseconds 100
    }
    # Evaluate the function call before applying the split operator. Inside
    # command argument mode -split is not the intended binary expression.
    $workspaceReceipt = Invoke-FixtureCli @('workspace', 'add', $workspace)
    $workspaceFields = $workspaceReceipt -split "\t", 2
    if ($workspaceFields.Count -ne 2 -or [string]::IsNullOrWhiteSpace($workspaceFields[0]) -or
        [string]::IsNullOrWhiteSpace($workspaceFields[1])) {
        throw 'workspace registration did not return an ID and path'
    }
    $workspaceId = $workspaceFields[0]
    $registered = @((Invoke-FixtureCli @('workspace', 'list')) -split "\r?\n" |
        Where-Object { ($_ -split "\t", 2)[0] -ceq $workspaceId })
    if ($registered.Count -ne 1) { throw 'registered workspace ID not found in catalog' }
    if ($Scenario -in @('MissingRunner', 'InvalidRunner', 'SilentRunner')) {
        $expected = switch ($Scenario) {
            'MissingRunner' { 'runner_io' }
            'InvalidRunner' { 'runner_identity_invalid' }
            'SilentRunner' { 'runner_startup_timeout' }
        }
        $null = Invoke-FixtureCli @('session', 'create', '--workspace', $workspaceId, '--provider', 'fake') $expected
        Track-RunnerChildren
        if ($runners.Count -ne 0 -or $daemon.HasExited -or $sentinel.HasExited) {
            throw 'failed startup leaked an owned process or affected an unrelated process'
        }
        $rows = Invoke-FixtureCli @('session', 'list')
        if ($rows -match '\tRunning(\t|$)') { throw 'failed startup reported a running session' }
        Write-Output "PASS: $Scenario fails closed without reporting Running"
        return
    }
    $first = New-FixtureSession $workspaceId
    if ($Scenario -in @('Interrupt', 'InterruptFailure', 'ActiveStop')) {
        $turn = Start-DeferredTurn $first.Session
        if ($Scenario -eq 'Interrupt') {
            $null = Invoke-FixtureCli @('interrupt', $first.Session, 'not-the-active-turn') 'TurnMismatch'
            $before = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
            if ($before -match '\tturn_completed\t') { throw 'wrong turn produced a terminal event' }
            $reply = Invoke-FixtureCli @('interrupt', $first.Session, $turn)
            if ($reply -ne (@('interrupted', $first.Session, $turn) -join [char]9)) {
                throw 'interrupt receipt mismatch'
            }
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            do {
                $timeline = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
                $terminals = @($timeline -split "\r?\n" | Where-Object { $_ -match '\tturn_completed\t' })
                if ($terminals.Count -gt 0) { break }
                Start-Sleep -Milliseconds 100
            } while ($deadline.Elapsed.TotalSeconds -lt 20)
            if ($terminals.Count -ne 1 -or $terminals[0] -notmatch '\tturn_completed\tInterrupted$') {
                throw 'expected exactly one Interrupted terminal event'
            }
        }
        if ($Scenario -eq 'InterruptFailure') {
            # Submit accepts the command into the runner queue, not proof of
            # an Interrupted terminal event. Observe the driver's actual error.
            $reply = Invoke-FixtureCli @('interrupt', $first.Session, $turn)
            if ($reply -ne (@('interrupted', $first.Session, $turn) -join [char]9)) {
                throw 'interrupt receipt mismatch'
            }
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            do {
                $timeline = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
                if ($timeline -match '\tturn_completed\t') { throw 'failed interrupt fabricated a terminal result' }
                if ($timeline -match '\tagent_error\t.*injected fake interrupt failure') { break }
                Start-Sleep -Milliseconds 100
            } while ($deadline.Elapsed.TotalSeconds -lt 20)
            if ($timeline -notmatch '\tagent_error\t.*injected fake interrupt failure') {
                throw 'driver interrupt failure was not observed'
            }
        }
        Assert-Stopped $first
        if ($Scenario -in @('ActiveStop', 'InterruptFailure')) {
            $timeline = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
            if ($timeline -notmatch "\tturn_uncertain\t$([regex]::Escape($turn))\t" -or
                $timeline -match '\tturn_completed\t') {
                throw 'stop during active turn fabricated a terminal result or lost uncertainty'
            }
        }
        if ($daemon.HasExited -or $sentinel.HasExited) { throw 'stop affected an unrelated process' }
        Write-Output "PASS: $Scenario produces the expected durable result"
        return
    }
    if ($Scenario -in @('CloseFailure', 'CloseBlocked')) {
        Assert-CompletedTurn $first.Session 'W1-D before failed close'
        Assert-UnconfirmedStop $first
        if ($daemon.HasExited -or $sentinel.HasExited) { throw 'failed close affected an unrelated process' }
        Write-Output "PASS: $Scenario remains StopUnconfirmed; no forced kill reported as success"
        return
    }
    $second = New-FixtureSession $workspaceId
    if ($first.Session -eq $second.Session -or $first.Process.Id -eq $second.Process.Id) {
        throw 'runner/session identity was reused'
    }
    Assert-CompletedTurn $first.Session 'W1-D first fixture prompt'
    Assert-CompletedTurn $second.Session 'W1-D second fixture prompt'
    Write-Output 'PASS: native daemon launches sibling runners in paths with spaces; both stream and complete'
    Assert-Stopped $first
    if ($second.Process.HasExited -or $sentinel.HasExited -or $daemon.HasExited) {
        throw 'stopping first runner affected another process'
    }
    Assert-CompletedTurn $second.Session 'W1-D second runner continues after stop'
    Assert-Stopped $second
    if ($sentinel.HasExited -or $daemon.HasExited) { throw 'session stop affected unrelated process' }
    Write-Output 'PASS: confirmed stop observes owned runner exit and leaves independent runner, daemon and sentinel alive'
} finally {
    try {
        Track-RunnerChildren
    } finally {
        # Stop only recorded handles, never use taskkill /IM or a persisted PID.
        try {
            foreach ($process in $runners.Values) { Stop-OwnedProcess $process }
        } finally {
            try { Stop-OwnedProcess $daemon } finally {
                try { Stop-OwnedProcess $sentinel } finally {
                    Remove-Item -LiteralPath $root -Recurse -Force
                }
            }
        }
    }
}
