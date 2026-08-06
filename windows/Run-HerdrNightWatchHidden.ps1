# Runs as the Windows Task Scheduler action. No console window is shown by the task action.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$settingsPath = 'HKCU:\Software\HerdrNachtwaechter'
$settings = Get-ItemProperty -Path $settingsPath -ErrorAction Stop
$distro = if ($settings.Distro) { [string]$settings.Distro } else { 'Ubuntu' }
$watcherPath = if ($settings.WatcherPath) { [string]$settings.WatcherPath } else { '/home/user/.codex/bin/herdr-night-watch.py' }
$wsl = "$env:SystemRoot\System32\wsl.exe"

& $wsl -d $distro --exec /usr/bin/python3 $watcherPath --watch
exit $LASTEXITCODE
