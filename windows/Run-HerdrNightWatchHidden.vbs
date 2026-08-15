' Compatibility launcher for installations created before the PowerShell task action.
' New installations run Run-HerdrNightWatchHidden.ps1 directly.
Option Explicit

Dim shell, exitCode, launcher, powershell
Set shell = CreateObject("WScript.Shell")

launcher = Left(WScript.ScriptFullName, Len(WScript.ScriptFullName) - 4) & ".ps1"
powershell = shell.ExpandEnvironmentStrings("%SystemRoot%") & "\System32\WindowsPowerShell\v1.0\powershell.exe"
exitCode = shell.Run("""" & powershell & """ -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File """ & launcher & """", 0, True)
WScript.Quit exitCode
