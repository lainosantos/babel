# Invoke explicitly from an Administrator PowerShell. Never auto-elevates.
[CmdletBinding()]
param([string]$PackageDirectory=$PSScriptRoot)
$ErrorActionPreference='Stop'
$package=(Resolve-Path -LiteralPath $PackageDirectory).Path
foreach ($name in @('BabelAudio.inf','BabelAudio.sys','BabelAudio.cat','babel-driver-installer.exe')) {
    if (!(Test-Path -LiteralPath (Join-Path $package $name) -PathType Leaf)) { throw "Missing package file: $name" }
}
& (Join-Path $package 'babel-driver-installer.exe') install --inf (Join-Path $package 'BabelAudio.inf')
if ($LASTEXITCODE -ne 0) { throw "Babel driver installation failed (exit $LASTEXITCODE)." }
