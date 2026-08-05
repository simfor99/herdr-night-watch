[CmdletBinding()]
param(
    [string]$TaskName = 'Herdr Night Watch',
    [string]$Distro = 'Ubuntu',
    [string]$WatcherPath = '/home/user/.codex/bin/herdr-night-watch.py'
)

$ErrorActionPreference = 'Stop'
$wsl = "$env:SystemRoot\System32\wsl.exe"
$watcher = $WatcherPath

$task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($task) {
    $info = Get-ScheduledTaskInfo -TaskName $TaskName
    Write-Host "Task: $($task.State), letzter Rückgabecode: $($info.LastTaskResult)"
}
else {
    Write-Host "Task '$TaskName' ist nicht installiert."
}

& $wsl -d $Distro --exec /usr/bin/python3 $watcher --status
exit $LASTEXITCODE
