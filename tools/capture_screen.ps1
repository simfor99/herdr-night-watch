param(
    [string]$FileName = "live_status_limits_docked.png"
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

Add-Type @'
using System;
using System.Runtime.InteropServices;

public struct RECT {
    public int Left;
    public int Top;
    public int Right;
    public int Bottom;
}

public class Win32 {
    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();

    [DllImport("user32.dll")]
    public static extern IntPtr FindWindow(string lpClassName, string lpWindowName);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
}
'@

[Win32]::SetProcessDPIAware()

$proc = Get-Process -Name 'Herdr-Nachtwaechter' | Where-Object { $_.MainWindowTitle -like '*Live*Status*' } | Select-Object -First 1
if ($proc) {
    $rect = New-Object RECT
    [Win32]::GetWindowRect($proc.MainWindowHandle, [ref]$rect)
    Write-Output "MainWindow Rect (DPI Aware): $($rect.Left), $($rect.Top), $($rect.Right), $($rect.Bottom)"

    $pad = 12
    $x = $rect.Left - $pad
    $y = $rect.Top - $pad
    $w = ($rect.Right - $rect.Left) + ($pad * 2)
    $h = [Math]::Min(1400, ($rect.Bottom - $rect.Top) + 400)

    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $graphics = [System.Drawing.Graphics]::FromImage($bmp)
    $graphics.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size($w, $h)))

    $outDir = "C:\Users\Simon\Desktop\temp"
    if (!(Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir }
    $outPath = Join-Path $outDir $FileName
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output "Saved screenshot to: $outPath"
    $graphics.Dispose()
    $bmp.Dispose()
} else {
    Write-Output "Could not find Live-Status process"
}
