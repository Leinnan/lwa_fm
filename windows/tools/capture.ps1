# Saves a picture of a process's main window (PrintWindow, so it works while the window is covered).
param([int]$ProcessId, [string]$Out)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Shot {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[Shot]::SetProcessDPIAware() | Out-Null
$process = Get-Process -Id $ProcessId
$handle = $process.MainWindowHandle
$rect = New-Object Shot+RECT
[Shot]::GetWindowRect($handle, [ref]$rect) | Out-Null
$width = $rect.R - $rect.L
$height = $rect.B - $rect.T
$bitmap = New-Object System.Drawing.Bitmap $width, $height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$dc = $graphics.GetHdc()
# 2 = PW_RENDERFULLCONTENT, needed for composition-drawn windows.
[Shot]::PrintWindow($handle, $dc, 2) | Out-Null
$graphics.ReleaseHdc($dc)
$bitmap.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
"$width x $height -> $Out"
