# First native lifecycle regression; no external provider or user data.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet w1d " + [guid]::NewGuid().ToString('N'))
$bin = Join-Path $root 'isolated binaries'
$runtime = Join-Path $root 'runtime'
$workspace = Join-Path $root 'workspace with spaces'
$runnerExe = Join-Path $bin 'fleet-runner.exe'
$daemon = $null
$sentinel = $null
$runners = @{}
$previous = @{}
# Inherit no fake fault flags or prior storage/catalog overrides.
$names = @('FLEET_RUNTIME_DIR', 'FLEET_DB_PATH', 'FLEET_RUNNER_BIN',
    'FLEET_ADOPT_LEGACY_DB', 'FLEET_OPERATION_KEY', 'FLEET_OPERATION_JOURNAL',
    'FLEET_CURSOR_JOURNAL', 'FLEET_SSH_CONFIG', 'FLEET_HOST_ID')
$names += @(Get-ChildItem Env: | Where-Object { $_.Name -like 'FLEET_FAKE_*' } |
    ForEach-Object { $_.Name })
foreach ($name in $names) {
    $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Start-OwnedProcess([string]$Executable, [string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
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

function Invoke-FixtureCli([string[]]$Arguments) {
    $process = Start-OwnedProcess (Join-Path $bin 'fleetctl.exe') $Arguments
    try {
        if (-not $process.WaitForExit(20000)) { throw "CLI timed out: $($Arguments[0])" }
        if ($process.ExitCode -ne 0) {
            $err = if ($process.FixtureStderr.IsCompletedSuccessfully) {
                $process.FixtureStderr.GetAwaiter().GetResult()
            } else { '<stderr unavailable>' }
            throw "CLI $($Arguments[0]) exit $($process.ExitCode): $err"
        }
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
    $env:FLEET_OPERATION_KEY = $key
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

New-Item -ItemType Directory -Path $root | Out-Null
try {
    foreach ($name in $names) {
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
    New-Item -ItemType Directory -Path $bin, $workspace | Out-Null
    foreach ($exe in @('fleetd.exe', 'fleet-runner.exe', 'fleetctl.exe')) {
        Copy-Item -LiteralPath (Join-Path $repo "target/debug/$exe") -Destination $bin
    }
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = Join-Path $root 'private data/fleet.sqlite'
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
    $workspaceId = (Invoke-FixtureCli @('workspace', 'add', $workspace) -split "\t")[0]
    $first = New-FixtureSession $workspaceId
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
                    foreach ($name in $names) {
                        [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process')
                    }
                    Remove-Item -LiteralPath $root -Recurse -Force
                }
            }
        }
    }
}
