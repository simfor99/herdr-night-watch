' Starts the WSL watchdog without creating a visible console window.
' This script is the Windows Task Scheduler action; it waits for the watcher
' so the scheduler can supervise and restart that single background process.
Option Explicit

Dim shell, exitCode
Set shell = CreateObject("WScript.Shell")

exitCode = shell.Run("""C:\WINDOWS\System32\wsl.exe"" -d Ubuntu --exec /usr/bin/python3 /home/user/.codex/bin/herdr-night-watch.py --watch", 0, True)
WScript.Quit exitCode
