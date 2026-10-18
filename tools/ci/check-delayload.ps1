<#
.SYNOPSIS
  Verifies that DirectML.dll is delay-loaded by the Windows release exe (DESIGN section 16.3, X-020).

.DESCRIPTION
  The statically linked ONNX Runtime always references DirectML. build.rs links /DELAYLOAD:DirectML.dll so
  the exe starts on systems without it. `dumpbin /imports` lists DirectML.dll in the normal import table when
  the delay-load is missing; `dumpbin /dependents` must not list it; and the "Section contains the following
  delay load imports" part must. Run from a developer prompt or let the script find dumpbin with vswhere.
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Exe)) { throw "Exe '$Exe' not found." }

function Find-Dumpbin {
    $cmd = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'dumpbin.exe not found and vswhere.exe is missing.' }
    $install = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $install) { throw 'No Visual Studio installation with the C++ tools found.' }
    $found = Get-ChildItem -Path (Join-Path $install 'VC\Tools\MSVC') -Recurse -Filter dumpbin.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1
    if (-not $found) { throw 'dumpbin.exe not found under the Visual Studio installation.' }
    return $found.FullName
}

$dumpbin = Find-Dumpbin
$dependents = & $dumpbin /nologo /dependents $Exe | Out-String
$imports = & $dumpbin /nologo /imports $Exe | Out-String

# 1. DirectML must not be an ordinary (load-time) dependent.
$sections = $dependents -split '(?i)Image has the following delay load dependencies'
if ($sections[0] -match '(?i)DirectML\.dll') {
    throw 'DirectML.dll is listed among the load-time dependents: /DELAYLOAD:DirectML.dll did not take effect.'
}
# 2. It must appear under the delay-load dependencies / imports.
if ($sections.Count -lt 2 -or $sections[1] -notmatch '(?i)DirectML\.dll') {
    throw 'DirectML.dll is not listed under the delay-load dependencies.'
}
$delayImports = $imports -split '(?i)Section contains the following delay load imports'
if ($delayImports.Count -lt 2 -or $delayImports[1] -notmatch '(?i)DirectML\.dll') {
    throw 'DirectML.dll is not listed under the delay-load imports.'
}
Write-Output 'check-delayload: ok (DirectML.dll is delay-loaded)'
