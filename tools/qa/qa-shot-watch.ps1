<#
.SYNOPSIS
  Screenshot watcher for the QA runner's `shot NAME` handshake (DESIGN section 15.3).

.DESCRIPTION
  The runner writes <Dir>\NAME.request (pid + window title) and waits for <Dir>\NAME.png. This
  script watches <Dir> for requests of the process -TargetPid and captures that process's main
  window with PrintWindow (a read-only window capture, the same code as tools\qa\shot.ps1).
  It injects no keyboard or mouse input and never changes the window. It exits when the target
  process exits.
#>
param(
    [Parameter(Mandatory = $true)][int]$TargetPid,
    [Parameter(Mandatory = $true)][string]$Dir,
    [int]$PollMs = 150
)
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class QaWin32Shot {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[QaWin32Shot]::SetProcessDPIAware() | Out-Null

function Save-Window([IntPtr]$handle, [string]$path) {
    $r = New-Object QaWin32Shot+RECT
    [QaWin32Shot]::GetWindowRect($handle, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
    if ($w -le 0 -or $h -le 0) { return $false }
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    # PW_RENDERFULLCONTENT (2): captures GPU-composited windows even when partly covered.
    $ok = [QaWin32Shot]::PrintWindow($handle, $hdc, 2)
    $g.ReleaseHdc($hdc)
    if (-not $ok) { $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size) }
    $tmp = "$path.tmp"
    $bmp.Save($tmp, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Move-Item -LiteralPath $tmp -Destination $path -Force
    return $true
}

while ($true) {
    $proc = Get-Process -Id $TargetPid -ErrorAction SilentlyContinue
    if (-not $proc -or $proc.HasExited) { break }
    foreach ($request in Get-ChildItem -LiteralPath $Dir -Filter '*.request' -ErrorAction SilentlyContinue) {
        $png = [System.IO.Path]::ChangeExtension($request.FullName, '.png')
        if (Test-Path -LiteralPath $png) { continue }
        try {
            $data = Get-Content -LiteralPath $request.FullName -Raw | ConvertFrom-Json
        } catch { continue }
        if ($data.pid -ne $TargetPid) { continue }
        $proc.Refresh()
        if ($proc.MainWindowHandle -ne 0) { Save-Window $proc.MainWindowHandle $png | Out-Null }
    }
    Start-Sleep -Milliseconds $PollMs
}
