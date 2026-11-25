$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet-w1a-" + [guid]::NewGuid().ToString('N'))
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$bin = Join-Path $repo 'target/debug'
$runtime = Join-Path $root 'runtime'
$daemon = $null
$runners = [System.Collections.Generic.List[Diagnostics.Process]]::new()
New-Item -ItemType Directory -Path $root | Out-Null

function Invoke-Fleetctl([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $bin 'fleetctl.exe'
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "fleetctl timed out: $($Arguments[0])"
        }
        $out = $stdout.GetAwaiter().GetResult()
        $err = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "fleetctl $($Arguments[0]) failed: $err" }
        return $out.Trim()
    } finally {
        $process.Dispose()
    }
}

function Track-Runners {
    foreach ($child in @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($daemon.Id)" |
        Where-Object { $_.Name -eq 'fleet-runner.exe' })) {
        if (@($runners | Where-Object { $_.Id -eq $child.ProcessId }).Count -eq 0) {
            $process = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
            if ($null -ne $process) {
                # Keep a handle to the original process, not just a reusable PID.
                [void]$process.Handle
                $runners.Add($process)
            }
        }
    }
}

function Assert-DaemonRejectsToken([string]$Token) {
    $address = ([IO.File]::ReadAllText((Join-Path $runtime 'fleetd.addr'))).Trim()
    $prefix = '\\.\pipe\'
    if (-not $address.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'daemon address must be a Windows named pipe'
    }
    $socket = [IO.Pipes.NamedPipeClientStream]::new('.', $address.Substring($prefix.Length), [IO.Pipes.PipeDirection]::InOut)
    try {
        $socket.Connect(5000)
        $stream = $socket
        $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $frame = @{ kind = 'request'; request_id = 'invalid'; request = @{
                type = 'hello'; client_id = 'w1a'; token = $Token
            } } | ConvertTo-Json -Compress
            $writer.WriteLine($frame)
            $writer.Flush()
            $line = $reader.ReadLineAsync()
            if (-not $line.Wait(5000)) { throw 'daemon did not reject token in time' }
            $response = $line.Result | ConvertFrom-Json
            if ($response.result.Err.code -ne 'Unauthorized') {
                throw 'daemon did not reject an invalid token'
            }
        } finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    } finally {
        $socket.Dispose()
    }
}

function Start-FixtureRunner([string]$Id) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $bin 'fleet-runner.exe'
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    foreach ($argument in @('--runner-id', $Id, '--session', "session-$Id",
                           '--provider', 'fake', '--workspace', $root)) {
        [void]$info.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::Start($info)
    [void]$process.Handle
    $runners.Add($process)
    $line = $process.StandardOutput.ReadLineAsync()
    if (-not $line.Wait(30000)) { throw "runner $Id did not start within 30 seconds" }
    if ([string]::IsNullOrEmpty($line.Result)) { throw "runner $Id exited without identity" }
    return ($line.Result | ConvertFrom-Json)
}

function Assert-RunnerToken($Identity, [string]$Token, [bool]$Valid) {
    $name = "fleet-runner-$($Identity.runner_id)"
    $address = ([IO.File]::ReadAllText((Join-Path $runtime "$name.addr"))).Trim()
    $prefix = '\\.\pipe\'
    if (-not $address.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'runner address must be a Windows named pipe'
    }
    $socket = [IO.Pipes.NamedPipeClientStream]::new('.', $address.Substring($prefix.Length), [IO.Pipes.PipeDirection]::InOut)
    try {
        $socket.Connect(5000)
        $stream = $socket
        $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false), 1024, $true)
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        try {
            $frame = @{ kind = 'request'; request_id = 'identity-test'; request = @{
                type = 'hello'; identity = $Identity; token = $Token
            } } | ConvertTo-Json -Compress -Depth 10
            $writer.WriteLine($frame)
            $writer.Flush()
            $line = $reader.ReadLineAsync()
            if (-not $line.Wait(5000)) { throw 'runner did not answer hello in time' }
            $response = $line.Result | ConvertFrom-Json
            if ($Valid) {
                if ($null -eq $response.result.Ok -or $response.result.Ok.type -ne 'hello') {
                    throw 'runner rejected its own valid identity and token'
                }
            } elseif ($response.result.Err.code -ne 'Unauthorized') {
                throw 'runner did not reject an invalid token'
            }
        } finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    } finally {
        $socket.Dispose()
    }
}

function Assert-PrivateFileAcl([string]$Path) {
    $ownerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) {
        throw "sensitive Fleet file inherits permissions: $([IO.Path]::GetFileName($Path))"
    }
    foreach ($rule in $acl.Access) {
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        $sid = $rule.IdentityReference.Translate(
            [Security.Principal.SecurityIdentifier]).Value
        if ($sid -ne $ownerSid) {
            throw "sensitive Fleet file grants another identity access: $([IO.Path]::GetFileName($Path))"
        }
    }
}

try {
    $env:FLEET_RUNTIME_DIR = $runtime
    $env:FLEET_DB_PATH = Join-Path $root 'data/fleet.sqlite'
    $env:FLEET_RUNNER_BIN = Join-Path $bin 'fleet-runner.exe'
    $daemon = Start-Process (Join-Path $bin 'fleetd.exe') -PassThru `
        -RedirectStandardOutput (Join-Path $root 'daemon.out') `
        -RedirectStandardError (Join-Path $root 'daemon.err')
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path (Join-Path $runtime 'fleetd.addr'))) {
        if ($daemon.HasExited -or $deadline.Elapsed.TotalSeconds -gt 30) {
            throw 'daemon did not start within 30 seconds'
        }
        Start-Sleep -Milliseconds 100
    }
    $daemonTokenPath = Join-Path $runtime 'fleetd.token'
    Assert-PrivateFileAcl (Join-Path $runtime 'fleetd.addr')
    Assert-PrivateFileAcl $daemonTokenPath
    $daemonToken = [IO.File]::ReadAllText($daemonTokenPath)
    $workspaceOutput = Invoke-Fleetctl @('workspace', 'add', $root)
    $workspace = ($workspaceOutput -split "`t")[0]
    $sessions = @()
    $runnerTokens = @()
    for ($i = 0; $i -lt 2; $i++) {
        try {
            $sessionOutput = Invoke-Fleetctl @('session', 'create', '--workspace', $workspace, '--provider', 'fake')
            $session = ($sessionOutput -split "`t")[0]
        } finally {
            Track-Runners
        }
        $sessions += $session
        if ([IO.File]::ReadAllText($daemonTokenPath) -ne $daemonToken) {
            throw "daemon token changed after runner $i started"
        }
        $runnerPath = Join-Path $runtime "fleet-runner-runner-$session.token"
        Assert-PrivateFileAcl $runnerPath
        $runnerToken = [IO.File]::ReadAllText($runnerPath)
        if ($runnerToken -eq $daemonToken -or $runnerTokens -contains $runnerToken) {
            throw 'runner token is not independent'
        }
        $runnerTokens += $runnerToken
        Assert-DaemonRejectsToken $runnerToken
        [void](Invoke-Fleetctl @('session', 'list'))
    }
    Assert-DaemonRejectsToken 'invalid-fixture-token'
    foreach ($session in $sessions) {
        [void](Invoke-Fleetctl @('prompt', $session, 'W1-A token isolation'))
    }
    foreach ($session in $sessions) {
        [void](Invoke-Fleetctl @('stop', $session))
    }
    $identity1 = Start-FixtureRunner 'w1a-one'
    $identity2 = Start-FixtureRunner 'w1a-two'
    $token1 = [IO.File]::ReadAllText((Join-Path $runtime 'fleet-runner-w1a-one.token'))
    $token2 = [IO.File]::ReadAllText((Join-Path $runtime 'fleet-runner-w1a-two.token'))
    Assert-PrivateFileAcl (Join-Path $runtime 'fleet-runner-w1a-one.token')
    Assert-PrivateFileAcl (Join-Path $runtime 'fleet-runner-w1a-two.token')
    Assert-RunnerToken $identity1 $token2 $false
    Assert-RunnerToken $identity2 $token1 $false
    Assert-RunnerToken $identity1 $token1 $true
    Assert-RunnerToken $identity2 $token2 $true
    # Both live standalone runners must use distinct durable databases, not
    # the endpoint directory. Their SQLite files inherit owner-only access.
    $ownerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    foreach ($id in @('w1a-one', 'w1a-two')) {
        $name = "fleet-runner-$id.spool.sqlite"
        $spoolPath = Join-Path (Split-Path -Parent $env:FLEET_DB_PATH) $name
        if (-not (Test-Path -LiteralPath $spoolPath -PathType Leaf)) {
            throw "durable runner spool missing: $name"
        }
        if (Test-Path -LiteralPath (Join-Path $runtime $name)) {
            throw "runner spool unexpectedly stored in runtime: $name"
        }
        $acl = Get-Acl -LiteralPath $spoolPath
        if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $ownerSid) {
            throw "spool owner mismatch: $name"
        }
        foreach ($rule in $acl.Access) {
            if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
                $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -ne $ownerSid) {
                throw "spool grants access to another SID: $name"
            }
        }
    }
    Write-Output 'PASS: distinct durable runner spools have owner-only access'
    Write-Output 'PASS: daemon and runner credentials are isolated and invalid tokens are rejected'
} finally {
    if ($null -ne $daemon -and -not $daemon.HasExited) {
        Track-Runners
        Stop-Process -Id $daemon.Id -ErrorAction SilentlyContinue
        $daemon.WaitForExit()
    }
    foreach ($runner in $runners) {
        try {
            if (-not $runner.HasExited) {
                $runner.Kill()
                [void]$runner.WaitForExit(5000)
            }
        } finally {
            $runner.Dispose()
        }
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction Stop
}
