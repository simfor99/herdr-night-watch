[CmdletBinding()]
param(
    [switch]$DryRun,
    [switch]$Demo,
    [int]$QuietSeconds = 5,
    [Nullable[int]]$WarningSeconds,
    [int]$PollSeconds = 1,
    [string]$TaskName = 'Herdr Night Watch',
    [string]$Distro = 'Ubuntu',
    [string]$WatcherPath = '/home/user/.codex/bin/herdr-night-watch.py'
)

$ErrorActionPreference = 'Stop'
$wsl = "$env:SystemRoot\System32\wsl.exe"
$watcher = $WatcherPath
$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction Stop

if ($task.State -eq 'Running') {
    throw "'$TaskName' is already running. Use the status or stop shortcut."
}

if ($Demo) {
    & $wsl -d $Distro --exec /usr/bin/python3 $watcher --demo
}
else {
    $armArguments = @('--arm', '--quiet-seconds', $QuietSeconds, '--poll-seconds', $PollSeconds)
    if ($PSBoundParameters.ContainsKey('WarningSeconds')) {
        $armArguments += @('--warning-seconds', $WarningSeconds.Value)
    }
    if ($DryRun) {
        $armArguments += '--dry-run'
    }
    & $wsl -d $Distro --exec /usr/bin/python3 $watcher @armArguments
}
if ($LASTEXITCODE -ne 0) {
    throw 'Night watch was not armed. No shutdown was scheduled.'
}

try {
    Start-ScheduledTask -TaskName $TaskName
}
catch {
    & $wsl -d $Distro --exec /usr/bin/python3 $watcher --cancel --cancel-source start_failed | Out-Null
    throw
}

Start-Sleep -Seconds 1
$info = Get-ScheduledTaskInfo -TaskName $TaskName
if ($Demo) {
    Write-Host 'Herdr-Nachtwächter-Demo läuft. Sie führt keinen Windows-Shutdown aus.'
}
else {
    $warningLabel = if ($PSBoundParameters.ContainsKey('WarningSeconds')) { "$WarningSeconds s" } else { 'gespeicherter Wert' }
    Write-Host "Herdr-Nachtwächter läuft. Ruhezeit: $QuietSeconds s, Warnfrist: $warningLabel."
}
Write-Host "Letzter Task-Status: $($info.LastTaskResult)"
