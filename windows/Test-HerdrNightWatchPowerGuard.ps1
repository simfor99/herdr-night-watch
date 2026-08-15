[CmdletBinding()]
param(
    [ValidateRange(1, 30)]
    [int]$HoldSeconds = 2
)

$ErrorActionPreference = 'Stop'

# This is a manual, non-destructive smoke test. It only sets the current
# PowerShell process' execution state; it never requests sleep or shutdown.
$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Bitte PowerShell als Administrator starten und den Smoke-Test erneut ausführen.'
}

$powerCfg = Join-Path $env:SystemRoot 'System32\powercfg.exe'
if (-not (Test-Path -LiteralPath $powerCfg)) {
    throw "powercfg.exe wurde nicht gefunden: $powerCfg"
}

if (-not ('HerdrNightWatch.NativePower' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace HerdrNightWatch {
    public static class NativePower {
        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern uint SetThreadExecutionState(uint esFlags);
    }
}
'@
}

# Windows PowerShell parses 0x80000000 as a signed negative integer first.
# Convert from the hexadecimal text explicitly so the native UInt32 flag is
# valid in both Windows PowerShell 5.1 and PowerShell 7.
$esContinuous = [Convert]::ToUInt32('80000000', 16)
$esSystemRequired = [uint32]0x00000001
$processName = [Diagnostics.Process]::GetCurrentProcess().ProcessName

function Get-PowerRequests {
    $lines = & $powerCfg /requests 2>&1
    $exitCode = $LASTEXITCODE
    $output = ($lines | Out-String)
    if ($exitCode -ne 0) {
        throw "powercfg /requests ist mit Code $exitCode fehlgeschlagen.`n$output"
    }
    return $output
}

function Get-SystemRequestBlock {
    param([Parameter(Mandatory = $true)][string]$Output)

    # powercfg keeps the SYSTEM section name stable across Windows locales.
    $match = [regex]::Match(
        $Output,
        '(?ims)^\s*SYSTEM:\s*(.*?)(?=^\s*[A-Z][A-Z_]+:\s*$|\z)'
    )
    if (-not $match.Success) {
        throw "powercfg /requests enthält keine auswertbare SYSTEM-Sektion.`n$Output"
    }
    return $match.Groups[1].Value
}

function Test-CurrentProcessRequest {
    param(
        [Parameter(Mandatory = $true)][string]$SystemBlock,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $escapedName = [regex]::Escape($Name)
    return $SystemBlock -match "(?im)^\s*\[PROCESS\].*$escapedName(?:\.exe)?"
}

$guardActivated = $false
$failure = $null

try {
    $result = [HerdrNightWatch.NativePower]::SetThreadExecutionState(
        $esContinuous -bor $esSystemRequired
    )
    if ($result -eq 0) {
        throw 'SetThreadExecutionState konnte den Energieschutz nicht aktivieren.'
    }
    $guardActivated = $true

    $systemBlock = Get-SystemRequestBlock (Get-PowerRequests)
    if (-not (Test-CurrentProcessRequest -SystemBlock $systemBlock -Name $processName)) {
        throw "Der aktuelle Prozess ($processName) erscheint trotz aktivem Guard nicht in SYSTEM.`n$systemBlock"
    }
    Write-Host "PASS: Energieschutz aktiv für $processName."
    Start-Sleep -Seconds $HoldSeconds
}
catch {
    $failure = $_.Exception
}
finally {
    if ($guardActivated) {
        try {
            $releaseResult = [HerdrNightWatch.NativePower]::SetThreadExecutionState($esContinuous)
            if ($releaseResult -eq 0) {
                throw 'SetThreadExecutionState konnte den Energieschutz nicht freigeben.'
            }
        }
        catch {
            if (-not $failure) {
                $failure = $_.Exception
            }
        }
    }
}

if ($failure) {
    Write-Error $failure.Message
    exit 1
}

try {
    $releasedBlock = Get-SystemRequestBlock (Get-PowerRequests)
    if (Test-CurrentProcessRequest -SystemBlock $releasedBlock -Name $processName) {
        throw "Der aktuelle Prozess ($processName) meldet nach der Freigabe weiterhin einen SYSTEM-Request.`n$releasedBlock"
    }
    Write-Host 'PASS: Energieschutz freigegeben.'
    exit 0
}
catch {
    Write-Error $_.Exception.Message
    exit 1
}
