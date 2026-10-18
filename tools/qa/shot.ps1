<#
.SYNOPSIS
  Launches a GUI exe, waits, captures its main window to a PNG (a read-only window capture).

.DESCRIPTION
  Same capture code as qa-shot-watch.ps1. No keyboard or mouse input is injected. Run it only against
  a sandbox build/home (see run-gui.ps1): the process is started with the current environment, so
  set USERPROFILE/HOME/CLAUDE_CONFIG_DIR to a sandbox in the calling shell first, or use
  run-gui.ps1 -Shots with a scenario instead. Only the process this script started is stopped
  (unless -KeepRunning).
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$DelayMs = 4000,
    [string]$ArgsLine = "",
    [switch]$KeepRunning
)
$ErrorActionPreference = 'Stop'
# Refuse to start the app outside a QA sandbox (DESIGN section 15): the home must carry the marker.
$sandboxHome = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
if (-not $sandboxHome -or -not (Test-Path -LiteralPath (Join-Path $sandboxHome '.qa-sandbox'))) {
    throw "USERPROFILE/HOME ('$sandboxHome') has no .qa-sandbox marker; refusing to start the app. Use run-gui.ps1 or set the sandbox environment first."
}
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Shot {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[Win32Shot]::SetProcessDPIAware() | Out-Null
if ($ArgsLine) { $p = Start-Process -FilePath $Exe -ArgumentList $ArgsLine -PassThru } else { $p = Start-Process -FilePath $Exe -PassThru }
$deadline = (Get-Date).AddMilliseconds($DelayMs + 20000)
do { Start-Sleep -Milliseconds 300; $p.Refresh() } while ($p.MainWindowHandle -eq 0 -and -not $p.HasExited -and (Get-Date) -lt $deadline)
if ($p.HasExited) { Write-Error "process exited early with code $($p.ExitCode)"; exit 1 }
Start-Sleep -Milliseconds $DelayMs
$h = $p.MainWindowHandle
$r = New-Object Win32Shot+RECT
[Win32Shot]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $hh = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [Win32Shot]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
if (-not $ok) { $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size) }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out ($w x $hh) pid=$($p.Id)"
if (-not $KeepRunning) { Stop-Process -Id $p.Id -Force }
