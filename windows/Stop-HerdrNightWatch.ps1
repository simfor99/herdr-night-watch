[CmdletBinding()]
param(
    [string]$TaskName = 'Herdr Night Watch',
    [string]$Distro = 'Ubuntu',
    [string]$WatcherPath = '/home/user/.codex/bin/herdr-night-watch.py',
    [string]$CancelSource = 'manual_stop_script'
)

$ErrorActionPreference = 'Stop'
$wsl = "$env:SystemRoot\System32\wsl.exe"
$watcher = $WatcherPath

& $wsl -d $Distro --exec /usr/bin/python3 $watcher --cancel --cancel-source $CancelSource
if ($LASTEXITCODE -ne 0) {
    throw 'The watcher could not be cancelled cleanly. Check its log before closing Windows.'
}

$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($task -and $task.State -eq 'Running') {
    Stop-ScheduledTask -TaskName $TaskName
}
Write-Host 'Herdr-Nachtwächter wurde gestoppt. Ein eigener ausstehender Shutdown wurde ebenfalls abgebrochen.'
