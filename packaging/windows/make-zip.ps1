<#
.SYNOPSIS
  Builds the Windows release zip (DESIGN sections 14.4, 16.3).

.DESCRIPTION
  claude-history-gui-v<version>-windows-x86_64.zip with claude-history-gui.exe, the four app-local VC++
  runtime DLLs (MSVCP140.dll, MSVCP140_1.dll, VCRUNTIME140.dll, VCRUNTIME140_1.dll, found with vswhere under
  VC\Redist\MSVC\<ver>\x64\Microsoft.VC<toolset>.CRT, e.g. VC143), README.txt, LICENSE and LICENSE-MIT, plus the .sha256 file.
  -CrtDir lets a test point at a directory that already holds the DLLs.

.EXAMPLE
  .\packaging\windows\make-zip.ps1 -Exe target\release\claude-history-gui.exe -Version 0.1.0 -OutDir dist
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [string]$CrtDir
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$crtDlls = 'MSVCP140.dll', 'MSVCP140_1.dll', 'VCRUNTIME140.dll', 'VCRUNTIME140_1.dll'

if (-not (Test-Path -LiteralPath $Exe)) { throw "Exe '$Exe' not found." }

function Find-CrtDir {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'vswhere.exe not found; pass -CrtDir.' }
    $install = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $install) { throw 'No Visual Studio installation with the C++ tools found.' }
    $redist = Join-Path $install 'VC\Redist\MSVC'
    # The newest redist version holding an x64 Microsoft.VC<toolset>.CRT directory (VC143 for
    # VS 2022; later toolsets keep the same VCRUNTIME140*.dll / MSVCP140*.dll names).
    $dir = Get-ChildItem -Path $redist -Directory -ErrorAction Stop |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+' } | Sort-Object Name -Descending |
        ForEach-Object {
            $x64 = Join-Path $_.FullName 'x64'
            if (Test-Path -LiteralPath $x64) {
                Get-ChildItem -Path $x64 -Directory -Filter 'Microsoft.VC*.CRT' | ForEach-Object { $_.FullName }
            }
        } | Select-Object -First 1
    if (-not $dir) { throw "No x64\Microsoft.VC*.CRT directory under $redist." }    return $dir
}

if (-not $CrtDir) { $CrtDir = Find-CrtDir }
foreach ($dll in $crtDlls) {
    if (-not (Test-Path -LiteralPath (Join-Path $CrtDir $dll))) { throw "$dll not found in $CrtDir." }
}

$artifact = "claude-history-gui-v$Version-windows-x86_64"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$outDir = (Resolve-Path $OutDir).Path
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("chg-zip-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    Copy-Item -LiteralPath $Exe -Destination (Join-Path $stage 'claude-history-gui.exe')
    foreach ($dll in $crtDlls) { Copy-Item -LiteralPath (Join-Path $CrtDir $dll) -Destination $stage }
    $readme = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'README.txt') -Raw).Replace('@VERSION@', $Version)
    Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Value $readme -NoNewline -Encoding utf8
    Copy-Item -LiteralPath (Join-Path $repo 'LICENSE') -Destination (Join-Path $stage 'LICENSE')
    Copy-Item -LiteralPath (Join-Path $repo 'crates\core\LICENSE-MIT') -Destination (Join-Path $stage 'LICENSE-MIT')

    $zip = Join-Path $outDir "$artifact.zip"
    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$zip.sha256" -Value "$hash  $artifact.zip" -Encoding ascii
    Write-Output $zip
} finally {
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
}
