$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$cli = Join-Path $repo 'target/debug/fleetctl.exe'
$root = Join-Path ([IO.Path]::GetTempPath()) ("fleet journal " + [guid]::NewGuid().ToString('N'))
$data = Join-Path $root 'data'
$journal = Join-Path $data 'operations.jsonl'
$oldCatalog = [Environment]::GetEnvironmentVariable('FLEET_SSH_CONFIG', 'Process')
$oldJournal = [Environment]::GetEnvironmentVariable('FLEET_OPERATION_JOURNAL', 'Process')

function Invoke-Cli([string[]]$Arguments, [bool]$Success = $true) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $cli
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { [void]$info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $out = $process.StandardOutput.ReadToEndAsync()
        $err = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) { throw 'CLI fixture timed out' }
        if (($process.ExitCode -eq 0) -ne $Success) {
            throw "unexpected CLI result: $($err.GetAwaiter().GetResult())"
        }
        return $out.GetAwaiter().GetResult()
    } finally {
        if (-not $process.HasExited) { $process.Kill(); [void]$process.WaitForExit(5000) }
        $process.Dispose()
    }
}

New-Item -ItemType Directory -Path $root | Out-Null
try {
    $env:FLEET_SSH_CONFIG = Join-Path $data 'hosts.json'
    $env:FLEET_OPERATION_JOURNAL = $journal
    $null = Invoke-Cli @('host', 'add', 'fixture', 'fixture@example.invalid')
    $null = Invoke-Cli @('host', 'add', 'other', 'other@example.invalid')
    $record = '{"host":"fixture","kind":"prompt","target":"ses_1","operation_key":"uncertain-1","created_at_unix_ms":1}'
    $other = '{"host":"other","kind":"answer","target":"approval_1","operation_key":"uncertain-2","created_at_unix_ms":2}'
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($journal, ($record + [char]10 + $other + [char]10), $utf8)
    $pending = Invoke-Cli @('--host', 'fixture', 'pending')
    if ($pending.Trim() -ne (@('fixture', 'prompt', 'ses_1', 'uncertain-1') -join [char]9)) {
        throw 'CLI did not recover only the selected host operation'
    }
    $clean = [IO.File]::ReadAllText($journal)
    [IO.File]::AppendAllText($journal, '{"host":', $utf8)
    $torn = [IO.File]::ReadAllText($journal)
    $output = Invoke-Cli @('pending') $false
    if ($output.Trim() -ne '' -or [IO.File]::ReadAllText($journal) -ne $torn) {
        throw 'CLI exposed partial result or changed the torn operation journal'
    }
    [IO.File]::WriteAllText($journal, $clean, $utf8)
    $alias = Join-Path $data 'linked.jsonl'
    New-Item -ItemType HardLink -Path $alias -Target $journal | Out-Null
    $output = Invoke-Cli @('pending') $false
    if ($output.Trim() -ne '' -or [IO.File]::ReadAllText($alias) -ne $clean) {
        throw 'CLI read or modified a hardlinked operation journal'
    }
    Write-Output 'PASS: CLI recovers host-scoped operation keys and rejects torn or linked journals'
} finally {
    [Environment]::SetEnvironmentVariable('FLEET_SSH_CONFIG', $oldCatalog, 'Process')
    [Environment]::SetEnvironmentVariable('FLEET_OPERATION_JOURNAL', $oldJournal, 'Process')
    Remove-Item -LiteralPath $root -Recurse -Force
}
