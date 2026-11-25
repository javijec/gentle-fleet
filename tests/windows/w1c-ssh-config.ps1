$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$cli = Join-Path $repo 'target/debug/fleetctl.exe'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet ssh catalog " + [guid]::NewGuid().ToString('N'))
$data = Join-Path $root 'data'
$catalog = Join-Path $data 'hosts.json'
$old = [Environment]::GetEnvironmentVariable('FLEET_SSH_CONFIG', 'Process')
$workers = [Collections.Generic.List[Diagnostics.Process]]::new()

function Start-Cli([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $cli
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$info.ArgumentList.Add($argument) }
    return [Diagnostics.Process]::Start($info)
}

function Finish-Cli([Diagnostics.Process]$Process) {
    $out = $Process.StandardOutput.ReadToEndAsync()
    $err = $Process.StandardError.ReadToEndAsync()
    if (-not $Process.WaitForExit(15000)) { throw 'CLI fixture timed out' }
    if ($Process.ExitCode -ne 0) { throw "CLI fixture failed: $($err.GetAwaiter().GetResult())" }
    return $out.GetAwaiter().GetResult()
}

New-Item -ItemType Directory -Path $root | Out-Null
try {
    $env:FLEET_SSH_CONFIG = $catalog
    for ($i = 0; $i -lt 8; $i++) {
        $workers.Add((Start-Cli @('host', 'add', "fixture-$i", 'fixture@example.invalid')))
    }
    foreach ($worker in $workers) { $null = Finish-Cli $worker }
    $hosts = @(Get-Content -LiteralPath $catalog -Raw | ConvertFrom-Json)
    if ($hosts.Count -ne 8) { throw 'concurrent catalog updates lost hosts' }
    for ($i = 0; $i -lt 8; $i++) {
        if (@($hosts | Where-Object { $_.alias -eq "fixture-$i" }).Count -ne 1) {
            throw 'host missing or duplicated'
        }
    }
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    foreach ($path in @($data, $catalog, "$catalog.lock")) {
        $acl = Get-Acl -LiteralPath $path
        if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $sid) {
            throw 'catalog owner differs from current SID'
        }
        if ($path -eq $data -and -not $acl.AreAccessRulesProtected) {
            throw 'catalog directory DACL is not protected'
        }
        foreach ($rule in $acl.Access) {
            if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
                $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value -ne $sid) {
                throw 'catalog grants access to another SID'
            }
        }
    }
    $remove = Start-Cli @('host', 'remove', 'fixture-0')
    $workers.Add($remove)
    $null = Finish-Cli $remove
    $list = Start-Cli @('host', 'list')
    $workers.Add($list)
    $output = Finish-Cli $list
    $remaining = @(Get-Content -LiteralPath $catalog -Raw | ConvertFrom-Json)
    if ($remaining.Count -ne 7 -or @($remaining | Where-Object { $_.alias -eq 'fixture-0' }).Count -ne 0 -or
        $output -match 'fixture-0') {
        throw 'catalog removal did not persist'
    }
    Write-Output 'PASS: concurrent host catalog updates, private ACL and durable removal'
} finally {
    foreach ($worker in $workers) {
        if (-not $worker.HasExited) { $worker.Kill(); [void]$worker.WaitForExit(5000) }
        $worker.Dispose()
    }
    [Environment]::SetEnvironmentVariable('FLEET_SSH_CONFIG', $old, 'Process')
    Remove-Item -LiteralPath $root -Recurse -Force
}
