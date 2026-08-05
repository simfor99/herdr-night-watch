' Starts the WSL watchdog without creating a visible console window.
' This script is the Windows Task Scheduler action; it waits for the watcher
' so the scheduler can supervise and restart that single background process.
Option Explicit

Dim shell, exitCode, distro, watcherPath, command
Set shell = CreateObject("WScript.Shell")

On Error Resume Next
distro = shell.RegRead("HKCU\Software\HerdrNachtwaechter\Distro")
If Err.Number <> 0 Then distro = "Ubuntu"
Err.Clear
watcherPath = shell.RegRead("HKCU\Software\HerdrNachtwaechter\WatcherPath")
If Err.Number <> 0 Then watcherPath = "/home/user/.codex/bin/herdr-night-watch.py"
On Error GoTo 0

command = """C:\WINDOWS\System32\wsl.exe"" -d """" & distro & """" --exec /usr/bin/python3 """" & watcherPath & """" --watch"
exitCode = shell.Run(command, 0, True)
WScript.Quit exitCode
