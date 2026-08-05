[CmdletBinding()]
param(
    [string]$TaskName = 'Herdr Night Watch',
    [string]$Distro = 'Ubuntu',
    [string]$CodexHome = '/home/user/.codex'
)

$ErrorActionPreference = 'Stop'
$watcher = "$CodexHome/bin/herdr-night-watch.py"
$wsl = "$env:SystemRoot\System32\wsl.exe"

if (-not (Test-Path $wsl)) {
    throw "wsl.exe was not found at $wsl."
}

& $wsl -d $Distro --exec /usr/bin/python3 $watcher --help | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "The watcher could not be started in WSL distro '$Distro'."
}

$hiddenLauncher = Join-Path $PSScriptRoot 'Run-HerdrNightWatchHidden.vbs'
if (-not (Test-Path $hiddenLauncher)) {
    throw "The hidden watcher launcher is missing: $hiddenLauncher"
}
$action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\wscript.exe" -Argument "`"$hiddenLauncher`""
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1)

Register-ScheduledTask `
    -TaskName $TaskName `
    -Action $action `
    -Principal $principal `
    -Settings $settings `
    -Description 'Fail-closed watcher for an armed Herdr night run.' `
    -Force | Out-Null

Write-Host "Installed hidden task '$TaskName'. The tray app is the only user interface."
