#Requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string]$BinaryDirectory, [string]$DriverDirectory)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if (!$IsWindows) { throw 'PE runtime inspection requires Windows.' }
$visualStudio = $env:BABEL_VSINSTALL
if (!$visualStudio) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -or !$visualStudio) { throw 'Visual Studio C++ build tools are unavailable.' }
}
$dumpbins=@(Get-ChildItem (Join-Path $visualStudio 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') | Sort-Object FullName -Descending)
if (!$dumpbins.Count) { throw 'MSVC dumpbin is unavailable.' }
$files = @('babel.exe', 'babel-tray.exe', 'babel-feedback.exe' | ForEach-Object { Join-Path $BinaryDirectory $_ })
if ($DriverDirectory) { $files += Join-Path $DriverDirectory 'babel-driver-installer.exe' }
foreach ($file in $files) {
    $imports=& $dumpbins[0].FullName /DEPENDENTS $file
    if ($LASTEXITCODE) { throw "Could not inspect PE imports: $file" }
    if (($imports -join "`n") -match '(?i)\b(vcruntime\w*|msvcp\d\w*|msvcr\d\w*|concrt\w*|ucrtbased)\.dll\b') {
        throw "Unbundled Visual C++ runtime dependency in $file. Build with +crt-static."
    }
    Write-Host "Verified no separate VC Redistributable is required: $file"
}
