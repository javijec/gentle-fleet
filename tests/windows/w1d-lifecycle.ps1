# Native lifecycle regressions; no external provider or user data.
param(
    [ValidateSet('All', 'Lifecycle', 'MissingRunner', 'InvalidRunner', 'SilentRunner', 'Interrupt', 'InterruptFailure', 'ActiveStop', 'CloseFailure', 'CloseBlocked', 'SlowClose', 'Recovery', 'RunnerLoss', 'RunnerLossLive', 'ApprovalRecovery', 'Acceptance')]
    [string]$Scenario = 'All'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($Scenario -eq 'All') {
    foreach ($case in @('Lifecycle', 'MissingRunner', 'InvalidRunner', 'SilentRunner', 'Interrupt', 'InterruptFailure', 'ActiveStop', 'CloseFailure', 'CloseBlocked', 'SlowClose')) {
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
    'FLEET_CURSOR_JOURNAL', 'FLEET_SSH_CONFIG', 'FLEET_HOST_ID',
    'FLEET_SUBSCRIBER_LIMIT', 'FLEET_TEST_FORWARD_DELAY_MS', 'FLEET_INPROCESS_RUNNER')
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
    $info.Environment['FLEET_CURSOR_JOURNAL'] = Join-Path $root 'private client/cursors.jsonl'
    $info.Environment['FLEET_OPERATION_JOURNAL'] = Join-Path $root 'private client/operations.jsonl'
    if ($null -ne $script:fixtureOperationKey) {
        $info.Environment['FLEET_OPERATION_KEY'] = $script:fixtureOperationKey
    }
    if ($info.Environment.ContainsKey('FLEET_RUNNER_BIN')) {
        throw 'fixture must not pass a runner binary override'
    }
    if ($Executable -eq (Join-Path $bin 'fleetd.exe')) {
        if ($Scenario -eq 'Acceptance') {
            $info.Environment['FLEET_FAKE_APPROVAL'] = '1'
            $info.Environment['FLEET_SUBSCRIBER_LIMIT'] = '1'
            $info.Environment['FLEET_TEST_FORWARD_DELAY_MS'] = '5000'
        }
        if ($Scenario -eq 'Recovery') {
            $info.Environment['FLEET_FAKE_SLOW_TURN_MS'] = '8000'
        }
        if ($Scenario -in @('RunnerLoss', 'RunnerLossLive', 'ApprovalRecovery')) {
            $info.Environment['FLEET_FAKE_APPROVAL'] = '1'
        }
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
        if ($Scenario -eq 'SlowClose') {
            $info.Environment['FLEET_FAKE_SLOW_CLOSE_MS'] = '1000'
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
            # Process exit and async pipe EOF are independent observations.
            # Wait for the actual diagnostic, with the same bounded drain
            # used for stdout. Never accept an absent or unexpected error.
            if (-not $process.FixtureStderr.Wait(5000)) {
                throw "CLI $($Arguments[0]) exit $($process.ExitCode): stderr did not close within 5s"
            }
            $err = $process.FixtureStderr.GetAwaiter().GetResult()
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

function Restart-FixtureDaemon {
    if ($null -ne $script:daemon) {
        Stop-OwnedProcess $script:daemon
        $script:daemon = $null
    }
    # Only this isolated fixture owns the old endpoint. Remove readiness
    # after the old daemon's exit, never touch the live runner endpoints.
    $addr = Join-Path $runtime 'fleetd.addr'
    if (Test-Path -LiteralPath $addr) { Remove-Item -LiteralPath $addr -Force }
    $script:daemon = Start-OwnedProcess (Join-Path $bin 'fleetd.exe') @()
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $addr)) {
        if ($script:daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'restarted daemon did not publish endpoint'
        }
        Start-Sleep -Milliseconds 100
    }
}

function Assert-ContiguousTimeline([string]$Timeline) {
    $expected = 1
    foreach ($line in ($Timeline -split "\r?\n")) {
        $fields = $line -split "\t", 2
        if ($fields.Count -ne 2 -or $fields[0] -ne [string]$expected) {
            throw "timeline has a duplicate or gap at sequence $expected"
        }
        $expected++
    }
}

function Assert-RecoveredTurn($Fixture, [string]$Key, [string]$Message) {
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    do {
        $timeline = Invoke-FixtureCli @('timeline', $Fixture.Session, '--since', '0', '--limit', '256')
        $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
            Where-Object { ($_ -split "\t")[0] -eq $Fixture.Session })
        $op = (Invoke-FixtureCli @('operation', $Key)) -split "\t"
        if ($rows.Count -eq 1 -and ($rows[0] -split "\t")[3] -eq 'Running' -and
            ($rows[0] -split "\t")[4] -eq 'Completed' -and $op.Count -ge 4 -and
            $op[3] -eq 'Completed' -and $timeline -match '\tturn_completed\tCompleted') { break }
        Start-Sleep -Milliseconds 100
    } while ($deadline.Elapsed.TotalSeconds -lt 30)
    if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'Running' -or
        ($rows[0] -split "\t")[4] -ne 'Completed' -or $op.Count -lt 4 -or $op[3] -ne 'Completed') {
        throw 'recovery did not restore the completed turn and operation'
    }
    Assert-ContiguousTimeline $timeline
    $lines = @($timeline -split "\r?\n")
    foreach ($pattern in @("\tuser\t$([regex]::Escape($Message))$", '\tdelta\tHello $',
        '\tdelta\tfrom fake$', '\ttool_completed\tfake_echo\ttrue$', '\tturn_completed\tCompleted$')) {
        if (@($lines | Where-Object { $_ -match $pattern }).Count -ne 1) {
            throw "recovered timeline missing or duplicated event: $pattern"
        }
    }
    if ($Fixture.Process.HasExited) { throw 'original runner exited during recovery' }
    Track-RunnerChildren
    if ($runners.Count -ne 1) { throw 'recovery launched a substitute runner' }
    # timeline prints seq, event_id, description; watch prints seq,
    # description. Compare every payload and sequence, not unequal schemas.
    $expectedReplay = @(foreach ($line in $lines) {
        $fields = $line -split "\t", 3
        if ($fields.Count -ne 3 -or $fields[1] -cne "$($Fixture.Session)-e$($fields[0])") {
            throw 'invalid durable event identity in timeline'
        }
        @($fields[0], $fields[2]) -join [char]9
    })
    $replay = Invoke-FixtureCli @('watch', $Fixture.Session, '--since', '0', '--count', [string]$lines.Count)
    $replayLines = @($replay -split "\r?\n")
    if ($replayLines.Count -ne $expectedReplay.Count) {
        throw "durable replay count differs: expected $($expectedReplay.Count), got $($replayLines.Count)"
    }
    for ($i = 0; $i -lt $expectedReplay.Count; $i++) {
        if ($replayLines[$i] -cne $expectedReplay[$i]) {
            throw "durable replay differs from timeline at sequence $($i + 1)"
        }
    }
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
    if ($Scenario -eq 'Acceptance') {
        $second = New-FixtureSession $workspaceId
        if ($first.Session -eq $second.Session -or $first.Process.Id -eq $second.Process.Id) {
            throw 'acceptance requires two distinct owned runners'
        }
        $probeExe = Join-Path $repo 'target/debug/examples/w1f_acceptance.exe'
        $probe = Start-OwnedProcess $probeExe @($first.Session, $second.Session)
        try {
            if (-not $probe.WaitForExit(60000)) { throw 'W1-F acceptance probe exceeded deadline' }
            if (-not $probe.FixtureStdout.Wait(5000) -or -not $probe.FixtureStderr.Wait(5000)) {
                throw 'W1-F probe output did not close'
            }
            Write-Output $probe.FixtureStdout.GetAwaiter().GetResult().Trim()
            if ($probe.ExitCode -ne 0) {
                throw "W1-F probe exit $($probe.ExitCode): $($probe.FixtureStderr.GetAwaiter().GetResult())"
            }
        } finally {
            Stop-OwnedProcess $probe
        }
        Assert-Stopped $first
        if ($second.Process.HasExited -or $daemon.HasExited -or $sentinel.HasExited) {
            throw 'acceptance stop affected an unrelated process'
        }
        Assert-Stopped $second
        Restart-FixtureDaemon
        $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
            Where-Object { ($_ -split "\t")[0] -in @($first.Session, $second.Session) })
        if ($rows.Count -ne 2 -or @($rows | Where-Object { ($_ -split "\t")[3] -ne 'Stopped' }).Count -ne 0) {
            throw 'acceptance stopped sessions were not durable'
        }
        Track-RunnerChildren
        if ($runners.Count -ne 2 -or $sentinel.HasExited) { throw 'unexpected runner or sentinel after restart' }
        Write-Output 'PASS: W1-F both owned runners stopped; restart did not revive them'
        return
    }
    if ($Scenario -in @('Recovery', 'RunnerLoss', 'RunnerLossLive', 'ApprovalRecovery')) {
        $key = [guid]::NewGuid().ToString('N')
        $script:fixtureOperationKey = $key
        $message = 'W1-E recovery fixture prompt'
        $receipt = (Invoke-FixtureCli @('prompt', $first.Session, $message)) -split "\t"
        if ($receipt.Count -ne 3 -or $receipt[1] -ne $first.Session -or -not $receipt[2]) {
            throw 'recovery prompt receipt mismatch'
        }
        $before = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
        if ($before -match '\tturn_completed\t') { throw 'turn completed before daemon fault' }
        $approvalId = $null
        if ($Scenario -ne 'Recovery') {
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            do {
                $pending = Invoke-FixtureCli @('approvals', '--session', $first.Session)
                $fields = $pending -split "\t"
                if ($fields.Count -eq 5 -and $fields[1] -eq $first.Session -and $fields[3] -eq 'pending') {
                    $approvalId = $fields[0]
                    break
                }
                Start-Sleep -Milliseconds 100
            } while ($deadline.Elapsed.TotalSeconds -lt 20)
            if (-not $approvalId) { throw 'pending approval was not observed' }
        }
        if ($Scenario -ne 'RunnerLossLive') {
            Stop-OwnedProcess $daemon
            $script:daemon = $null
        }
        if ($first.Process.HasExited -or $sentinel.HasExited) { throw 'original runner or sentinel exited before fault' }
        if ($Scenario -eq 'Recovery') {
            $probeExe = Join-Path $repo 'target/debug/examples/w1e_spool_probe.exe'
            $spool = Join-Path $root "private data/fleet-runner-runner-$($first.Session).spool.sqlite"
            $probe = Start-OwnedProcess $probeExe @($spool, $receipt[2], $key)
            try {
                if (-not $probe.WaitForExit(20000) -or $probe.ExitCode -ne 0) {
                    throw 'offline spool probe did not observe an unacknowledged terminal'
                }
                Write-Output 'PASS: terminal committed to original runner spool while daemon was absent'
            } finally {
                Stop-OwnedProcess $probe
            }
        }
        if ($Scenario -in @('RunnerLoss', 'RunnerLossLive')) {
            $first.Process.Kill()
            if (-not $first.Process.WaitForExit(5000)) { throw 'owned runner did not terminate' }
        }
        if ($Scenario -eq 'RunnerLossLive') {
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            do {
                $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
                    Where-Object { ($_ -split "\t")[0] -eq $first.Session })
                $approval = (Invoke-FixtureCli @('approval', $approvalId)) -split "\t"
                if ($rows.Count -eq 1 -and ($rows[0] -split "\t")[3] -eq 'NeedsReconciliation' -and
                    $approval.Count -ge 4 -and $approval[3] -eq 'uncertain') { break }
                Start-Sleep -Milliseconds 100
            } while ($deadline.Elapsed.TotalSeconds -lt 20)
            if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'NeedsReconciliation' -or
                $approval.Count -lt 4 -or $approval[3] -ne 'uncertain') {
                throw 'live daemon failed to classify lost runner and approval'
            }
        }
        Restart-FixtureDaemon
        if ($Scenario -in @('RunnerLoss', 'RunnerLossLive')) {
            $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
                Where-Object { ($_ -split "\t")[0] -eq $first.Session })
            if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'NeedsReconciliation') {
                throw 'lost runner did not leave session NeedsReconciliation'
            }
            $approval = (Invoke-FixtureCli @('approval', $approvalId)) -split "\t"
            if ($approval.Count -lt 4 -or $approval[3] -ne 'uncertain') {
                throw 'lost runner approval was not classified uncertain'
            }
            $script:fixtureOperationKey = [guid]::NewGuid().ToString('N')
            $null = Invoke-FixtureCli @('prompt', $first.Session, 'must not execute') 'reconciliation|runner|session'
            $script:fixtureOperationKey = [guid]::NewGuid().ToString('N')
            $null = Invoke-FixtureCli @('answer', $approvalId, 'once') 'NeedsReconciliation'
            Track-RunnerChildren
            if ($runners.Count -ne 1 -or -not $first.Process.HasExited -or $sentinel.HasExited) {
                throw 'recovery created a substitute runner or affected sentinel'
            }
            Write-Output 'PASS: lost runner remains uncertain; no prompt, approval or replacement dispatched'
            return
        }
        if ($Scenario -eq 'ApprovalRecovery') {
            $approval = (Invoke-FixtureCli @('approval', $approvalId)) -split "\t"
            if ($approval.Count -lt 4 -or $approval[3] -ne 'pending') {
                throw 'pending approval was changed without an explicit decision'
            }
            $script:fixtureOperationKey = [guid]::NewGuid().ToString('N')
            $answerKey = $script:fixtureOperationKey
            $null = Invoke-FixtureCli @('answer', $approvalId, 'once')
            $null = Invoke-FixtureCli @('answer', $approvalId, 'once')
        }
        Assert-RecoveredTurn $first $key $message
        if ($Scenario -eq 'ApprovalRecovery') {
            $approval = (Invoke-FixtureCli @('approval', $approvalId)) -split "\t"
            $timeline = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256')
            if ($approval[3] -ne 'accepted' -or @($timeline -split "\r?\n" |
                Where-Object { $_ -match '\tapproval_resolved\t' }).Count -ne 1) {
                throw 'approval recovery duplicated or lost the explicit decision'
            }
            $op = (Invoke-FixtureCli @('operation', $answerKey)) -split "\t"
            if ($op.Count -lt 4 -or $op[3] -ne 'Completed') { throw 'approval decision not durable' }
        }
        Assert-Stopped $first
        if ($daemon.HasExited -or $sentinel.HasExited) { throw 'reattached stop affected unrelated process' }
        Restart-FixtureDaemon
        $rows = @((Invoke-FixtureCli @('session', 'list')) -split "\r?\n" |
            Where-Object { ($_ -split "\t")[0] -eq $first.Session })
        if ($rows.Count -ne 1 -or ($rows[0] -split "\t")[3] -ne 'Stopped') {
            throw 'stopped session was revived after daemon restart'
        }
        Track-RunnerChildren
        if ($runners.Count -ne 1 -or $sentinel.HasExited) { throw 'unexpected runner or sentinel exit' }
        Write-Output "PASS: $Scenario authenticated replay and confirmed stop after reattach"
        return
    }
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
    if ($Scenario -eq 'SlowClose') {
        Assert-CompletedTurn $first.Session 'W1-D before slow close'
        Assert-Stopped $first
        if ($daemon.HasExited -or $sentinel.HasExited) { throw 'slow close affected an unrelated process' }
        Write-Output 'PASS: slow close confirms runner exit after a 1000ms driver close'
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
    # Exercise the same public JSONL path used by the real W2 provider smoke.
    # No new model call; use the already completed FakeDriver timeline.
    $jsonTimeline = Invoke-FixtureCli @('timeline', $first.Session, '--since', '0', '--limit', '256', '--json')
    $jsonRows = @($jsonTimeline -split "\r?\n" | ForEach-Object { $_ | ConvertFrom-Json })
    if ($jsonRows.Count -eq 0) { throw 'JSONL timeline missing' }
    for ($i = 0; $i -lt $jsonRows.Count; $i++) {
        if ($jsonRows[$i].session_id -ne $first.Session -or $jsonRows[$i].seq -ne ($i + 1)) {
            throw 'JSONL timeline identity or sequence mismatch'
        }
    }
    $jsonReplay = Invoke-FixtureCli @('watch', $first.Session, '--since', '0', '--count', "$($jsonRows.Count)", '--json')
    if ($jsonReplay -cne $jsonTimeline) { throw 'JSONL replay differs from durable timeline' }
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
