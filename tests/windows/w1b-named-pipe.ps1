$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-w1b-" + [guid]::NewGuid().ToString('N'))
$runtime = Join-Path $root 'runtime'
$daemon = $null
$competitor = $null
$previousRuntime = [Environment]::GetEnvironmentVariable('FLEET_RUNTIME_DIR', 'Process')
$previousDb = [Environment]::GetEnvironmentVariable('FLEET_DB_PATH', 'Process')

# Start console executables without attaching or creating a console window.
# Drain both streams asynchronously so redirected output cannot block startup.
# Keep diagnostics with the actual Process object, including early exits.
function Start-FixtureDaemon {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $bin 'fleetd.exe'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    $process | Add-Member -NotePropertyName FixtureStdout -NotePropertyValue ($process.StandardOutput.ReadToEndAsync())
    $process | Add-Member -NotePropertyName FixtureStderr -NotePropertyValue ($process.StandardError.ReadToEndAsync())
    return $process
}

function Get-FixtureDiagnostics([Diagnostics.Process]$Process) {
    if ($Process.HasExited) {
        $state = "exit=$($Process.ExitCode) (0x$($Process.ExitCode.ToString('X8')))"
    } else {
        $state = 'still running'
    }
    # A descendant could inherit a redirected pipe. Do not wait indefinitely
    # for EOF just to print diagnostics.
    $stderr = if ($Process.FixtureStderr.IsCompletedSuccessfully) {
        $Process.FixtureStderr.GetAwaiter().GetResult()
    } else { '<stderr not yet available>' }
    $stdout = if ($Process.FixtureStdout.IsCompletedSuccessfully) {
        $Process.FixtureStdout.GetAwaiter().GetResult()
    } else { '<stdout not yet available>' }
    return "$state; daemon stderr: $stderr; daemon stdout: $stdout"
}

function Assert-FixtureRejected([Diagnostics.Process]$Process, [string]$Context) {
    # A Rust main returning Err exits with 1. External console termination
    # (STATUS_CONTROL_C_EXIT), crashes and successful exits are not rejection.
    if (-not $Process.HasExited -or $Process.ExitCode -ne 1) {
        throw "$Context did not exit with the expected error: $(Get-FixtureDiagnostics $Process)"
    }
}

New-Item -ItemType Directory -Path $root | Out-Null
try {
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = Join-Path $root 'data/fleet.sqlite'
    $daemon = Start-FixtureDaemon
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    $addressFile = Join-Path $runtime 'fleetd.addr'
    while (-not (Test-Path $addressFile)) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw "daemon failed to publish endpoint within 30 seconds: $(Get-FixtureDiagnostics $daemon)"
        }
        Start-Sleep -Milliseconds 100
    }
    $address = ([IO.File]::ReadAllText($addressFile)).Trim()
    if (-not $address.StartsWith('\\.\pipe\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Windows daemon endpoint is not a named pipe'
    }
    # Reject inherited or broadly writable credential/endpoint ACLs.
    # The exact owner SID is derived at runtime (no locale-dependent group names).
    $ownerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    foreach ($sensitive in @($runtime, $addressFile, (Join-Path $runtime 'fleetd.token'))) {
        $acl = Get-Acl -LiteralPath $sensitive
        if (-not $acl.AreAccessRulesProtected) {
            throw "Fleet endpoint file inherits unprotected ACL: $([IO.Path]::GetFileName($sensitive))"
        }
        foreach ($rule in $acl.Access) {
            if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
            $sid = $rule.IdentityReference.Translate(
                [Security.Principal.SecurityIdentifier]).Value
            if ($sid -ne $ownerSid) {
                throw "Fleet endpoint file grants non-owner access: $([IO.Path]::GetFileName($sensitive))"
            }
        }
    }
    $runtimeAcl = Get-Acl -LiteralPath $runtime
    if ($runtimeAcl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $ownerSid) {
        throw 'runtime directory owner is not the current SID'
    }
    # A separate daemon must refuse an existing inherited-ACL runtime.
    $unsafeRuntime = Join-Path $root 'unsafe-runtime'
    New-Item -ItemType Directory -Path $unsafeRuntime | Out-Null
    $env:FLEET_RUNTIME_DIR = $unsafeRuntime
    $originalDb = $env:FLEET_DB_PATH
    $env:FLEET_DB_PATH = Join-Path $root 'unsafe-data/fleet.sqlite'
    $unsafeDaemon = Start-FixtureDaemon
    try {
        if (-not $unsafeDaemon.WaitForExit(10000)) {
            throw 'daemon accepted an unsafe runtime directory'
        }
        Assert-FixtureRejected $unsafeDaemon 'unsafe runtime'
        if (Test-Path (Join-Path $unsafeRuntime 'fleetd.addr')) {
            throw 'daemon did not fail closed for unsafe runtime'
        }
    } finally {
        if (-not $unsafeDaemon.HasExited) { $unsafeDaemon.Kill(); $unsafeDaemon.WaitForExit() }
        $unsafeDaemon.Dispose()
        $env:FLEET_RUNTIME_DIR = $runtime
        $env:FLEET_DB_PATH = $originalDb
    }
    $name = $address.Substring('\\.\pipe\'.Length)
    $client = [IO.Pipes.NamedPipeClientStream]::new('.', $name, [IO.Pipes.PipeDirection]::InOut)
    try {
        $client.Connect(5000)
        $writer = [IO.StreamWriter]::new($client, [Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [IO.StreamReader]::new($client, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $writer.WriteLine('{"kind":"request","request_id":"invalid","request":{"type":"hello","client_id":"w1b","token":"invalid-fixture-token"}}')
            $writer.Flush()
            $line = $reader.ReadLineAsync()
            if (-not $line.Wait(5000)) { throw 'daemon did not respond to invalid token' }
            $response = $line.Result | ConvertFrom-Json
            if ($response.result.Err.code -ne 'Unauthorized') {
                throw 'daemon did not explicitly reject invalid credentials'
            }
        } finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    } finally {
        $client.Dispose()
    }
    # Exercise actual SSH bridge half-close over the named pipe:
    # EOF on stdin must not discard the pending fleetd responses.
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $bin 'fleet-ssh-bridge.exe'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $bridge = [Diagnostics.Process]::Start($start)
    try {
        $bridge.StandardInput.WriteLine('{"kind":"request","request_id":"h1","request":{"type":"hello","client_id":"w1b","token":null}}')
        $bridge.StandardInput.WriteLine('{"kind":"request","request_id":"a1","request":{"type":"list_workspaces"}}')
        $bridge.StandardInput.Close()
        $outTask = $bridge.StandardOutput.ReadToEndAsync()
        $errTask = $bridge.StandardError.ReadToEndAsync()
        if (-not $bridge.WaitForExit(15000)) {
            $bridge.Kill()
            [void]$bridge.WaitForExit(5000)
            $partial = $outTask.GetAwaiter().GetResult()
            $diagnostics = $errTask.GetAwaiter().GetResult()
            $responseIds = @($partial -split "\r?\n" | Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object {
                try { ($_ | ConvertFrom-Json).request_id } catch { '<invalid JSONL>' }
            }) -join ','
            throw "SSH bridge did not exit after EOF (response IDs: $responseIds; diagnostics: $diagnostics)"
        }
        $output = $outTask.GetAwaiter().GetResult()
        $errors = $errTask.GetAwaiter().GetResult()
        if ($bridge.ExitCode -ne 0) {
            throw "bridge exited unsuccessfully after EOF: $errors"
        }
        $frames = @($output -split "\r?\n" | Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object { $_ | ConvertFrom-Json })
        if (@($frames | Where-Object { $_.request_id -eq 'h1' -and $_.result.Ok.type -eq 'hello' }).Count -ne 1) {
            throw 'SSH bridge dropped Hello response after half-close'
        }
        if (@($frames | Where-Object { $_.request_id -eq 'a1' -and $_.result.Ok.type -eq 'workspaces' }).Count -ne 1) {
            throw 'SSH bridge dropped Workspaces response after half-close'
        }
    } finally {
        if (-not $bridge.HasExited) {
            $bridge.Kill()
            [void]$bridge.WaitForExit(5000)
        }
        $bridge.Dispose()
    }

    $env:FLEET_DB_PATH = Join-Path $root 'competitor/fleet.sqlite'
    $competitor = Start-FixtureDaemon
    if (-not $competitor.WaitForExit(10000)) {
        throw 'second daemon did not reject the active endpoint within 10 seconds'
    }
    Assert-FixtureRejected $competitor 'endpoint competitor'
    if (Test-Path -LiteralPath $env:FLEET_DB_PATH) {
        throw 'endpoint competitor opened SQLite before reserving the endpoint'
    }
    if ($daemon.HasExited) { throw 'first daemon exited after endpoint collision' }
    if (([IO.File]::ReadAllText($addressFile)).Trim() -ne $address) {
        throw 'second daemon replaced the active endpoint'
    }
    # A junction in an ancestor must be rejected before endpoint creation.
    $junctionTarget = Join-Path $root 'junction-target'
    $junction = Join-Path $root 'junction'
    New-Item -ItemType Directory -Path $junctionTarget | Out-Null
    New-Item -ItemType Junction -Path $junction -Target $junctionTarget | Out-Null
    try {
        if (-not ((Get-Item -LiteralPath $junction).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'fixture did not create a real reparse point'
        }
        $env:FLEET_RUNTIME_DIR = Join-Path $junction 'runtime'
        $env:FLEET_DB_PATH = Join-Path $root 'junction-data/fleet.sqlite'
        $junctionDaemon = Start-FixtureDaemon
        try {
            if (-not $junctionDaemon.WaitForExit(10000)) {
                throw 'daemon accepted a runtime through an ancestor junction'
            }
            Assert-FixtureRejected $junctionDaemon 'ancestor junction'
            if (Test-Path (Join-Path $junctionTarget 'runtime/fleetd.addr')) {
                throw 'daemon did not fail closed for ancestor reparse point'
            }
        } finally {
            if (-not $junctionDaemon.HasExited) { $junctionDaemon.Kill(); [void]$junctionDaemon.WaitForExit(5000) }
            $junctionDaemon.Dispose()
        }
    } finally {
        # Delete the junction itself, never recursively traverse its target.
        [IO.Directory]::Delete($junction)
        $env:FLEET_RUNTIME_DIR = $runtime
    }
    Write-Output 'PASS: runtime rejects ancestor junction'
    # The same physical directory reached with an alternate spelling must
    # not reserve a distinct pipe and replace the original credentials.
    $aliasRuntime = Join-Path $root 'RUNTIME'
    if (-not [IO.File]::Exists((Join-Path $aliasRuntime 'fleetd.addr'))) {
        throw 'fixture requires a case-insensitive alias of the same runtime'
    }
    $env:FLEET_RUNTIME_DIR = $aliasRuntime
    $env:FLEET_DB_PATH = Join-Path $root 'alias-competitor/fleet.sqlite'
    $tokenFile = Join-Path $runtime 'fleetd.token'
    $originalToken = [IO.File]::ReadAllText($tokenFile)
    $aliasDaemon = Start-FixtureDaemon
    try {
        if (-not $aliasDaemon.WaitForExit(10000)) {
            throw 'alternate runtime spelling bypassed endpoint reservation'
        }
        Assert-FixtureRejected $aliasDaemon 'alternate runtime competitor'
        if ($daemon.HasExited -or
            ([IO.File]::ReadAllText($addressFile)).Trim() -ne $address -or
            [IO.File]::ReadAllText($tokenFile) -ne $originalToken) {
            throw 'alternate runtime spelling affected the active endpoint'
        }
    } finally {
        if (-not $aliasDaemon.HasExited) { $aliasDaemon.Kill(); [void]$aliasDaemon.WaitForExit(5000) }
        $aliasDaemon.Dispose()
        $env:FLEET_RUNTIME_DIR = $runtime
    }
    Write-Output 'PASS: alternate runtime spelling cannot replace active endpoint'
    # An unclean shutdown leaves both files behind. The next daemon must
    # replace them only after reserving the endpoint, and remain usable.
    $daemon.Kill()
    if (-not $daemon.WaitForExit(10000)) { throw 'daemon did not exit after kill' }
    $daemon.Dispose()
    $daemon = $null
    if (-not (Test-Path $addressFile) -or -not (Test-Path (Join-Path $runtime 'fleetd.token'))) {
        throw 'fixture did not retain stale endpoint files'
    }
    $env:FLEET_DB_PATH = $originalDb
    $daemon = Start-FixtureDaemon
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    $restarted = $false
    $lastProbe = 'no authenticated probe attempted'
    # Do not connect using the old address while the new daemon holds a
    # reserved pipe but has not completed startup. Require both publication
    # and rotation of the token, then prove authentication by actual IPC.
    while ($deadline.Elapsed.TotalSeconds -lt 30 -and -not $daemon.HasExited) {
        $ready = $false
        try {
            $ready = (Test-Path -LiteralPath $addressFile) -and
                (Test-Path -LiteralPath $tokenFile) -and
                ([IO.File]::ReadAllText($tokenFile) -ne $originalToken)
        } catch {
            # A file may be transiently unavailable during publication.
            $ready = $false
        }
        if (-not $ready) {
            Start-Sleep -Milliseconds 100
            continue
        }
        # A successful authenticated bridge must identify the new process.
        $probe = [Diagnostics.ProcessStartInfo]::new()
        $probe.FileName = Join-Path $bin 'fleet-ssh-bridge.exe'
        $probe.UseShellExecute = $false
        $probe.CreateNoWindow = $true
        $probe.RedirectStandardInput = $true
        $probe.RedirectStandardOutput = $true
        $probe.RedirectStandardError = $true
        $process = [Diagnostics.Process]::Start($probe)
        try {
            $process.StandardInput.WriteLine('{"kind":"request","request_id":"restart","request":{"type":"hello","client_id":"w1b-restart","token":null}}')
            $process.StandardInput.Close()
            $outputTask = $process.StandardOutput.ReadToEndAsync()
            $errorTask = $process.StandardError.ReadToEndAsync()
            if (-not $process.WaitForExit(3000)) {
                $process.Kill()
                [void]$process.WaitForExit(5000)
                $lastProbe = 'bridge timed out after publication'
            } elseif ($process.ExitCode -eq 0) {
                $frames = @($outputTask.GetAwaiter().GetResult() -split "\r?\n" |
                    Where-Object { $_.Trim().Length -gt 0 } | ForEach-Object { $_ | ConvertFrom-Json })
                $hello = @($frames | Where-Object {
                    $_.request_id -eq 'restart' -and $_.result.PSObject.Properties.Name -contains 'Ok' -and
                    $_.result.Ok.type -eq 'hello' -and $_.result.Ok.server_id -eq "fleetd-$($daemon.Id)"
                })
                if ($hello.Count -eq 1) {
                    $restarted = $true
                } else {
                    $lastProbe = 'bridge did not return Hello for the restarted daemon PID'
                }
            } else {
                $lastProbe = "bridge exit $($process.ExitCode): $($errorTask.GetAwaiter().GetResult())"
            }
        } finally {
            if (-not $process.HasExited) { $process.Kill(); [void]$process.WaitForExit(5000) }
            $process.Dispose()
        }
        if (-not $restarted) { Start-Sleep -Milliseconds 100 } else { break }
    }
    if (-not $restarted) {
        # Never print token or raw Hello frames.
        throw "daemon did not authenticate after restart with stale endpoints; last probe: $lastProbe; $(Get-FixtureDiagnostics $daemon)"
    }
    Write-Output 'PASS: stale endpoint files replaced after unclean daemon restart'
    Write-Output 'PASS: named pipe endpoint rejects invalid token and active endpoint collision'
} finally {
    if ($null -ne $competitor) {
        if (-not $competitor.HasExited) {
            $competitor.Kill()
            [void]$competitor.WaitForExit(5000)
        }
        $competitor.Dispose()
    }
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        $daemon.Kill()
        [void]$daemon.WaitForExit(5000)
    }
    if ($null -ne $daemon) { $daemon.Dispose() }
    [Environment]::SetEnvironmentVariable('FLEET_RUNTIME_DIR', $previousRuntime, 'Process')
    [Environment]::SetEnvironmentVariable('FLEET_DB_PATH', $previousDb, 'Process')
    Remove-Item -LiteralPath $root -Recurse -Force
}
