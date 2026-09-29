#Requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string]$BinaryDirectory, [Parameter(Mandatory)][string]$DriverDirectory)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if (!$IsWindows -or !$env:BABEL_VSINSTALL) { throw 'Run WDK setup on Windows first.' }
$dumpbins=@(Get-ChildItem (Join-Path $env:BABEL_VSINSTALL 'VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe') | Sort-Object FullName -Descending)
if (!$dumpbins.Count) { throw 'MSVC dumpbin is unavailable.' }
foreach ($file in @((Join-Path $BinaryDirectory 'babel.exe'),(Join-Path $BinaryDirectory 'babel-tray.exe'),(Join-Path $DriverDirectory 'babel-driver-installer.exe'))) {
    $imports=& $dumpbins[0].FullName /DEPENDENTS $file
    if ($LASTEXITCODE) { throw "Could not inspect PE imports: $file" }
    if (($imports -join "`n") -match '(?i)\b(vcruntime\w*|msvcp\d\w*|msvcr\d\w*|concrt\w*|ucrtbased)\.dll\b') {
        throw "Unbundled Visual C++ runtime dependency in $file. Build with +crt-static."
    }
    Write-Host "Verified no separate VC Redistributable is required: $file"
}
