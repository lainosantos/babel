# Removes only the exact Babel driver through the ownership-checking Rust helper.
[CmdletBinding()]
param([string]$PackageDirectory=$PSScriptRoot)
$ErrorActionPreference='Stop'
$package=(Resolve-Path -LiteralPath $PackageDirectory).Path
$helper=Join-Path $package 'babel-driver-installer.exe'
if (!(Test-Path -LiteralPath $helper -PathType Leaf)) { throw 'Missing Babel driver installer.' }
& $helper remove --inf (Join-Path $package 'BabelAudio.inf')
if ($LASTEXITCODE -ne 0) { throw "Babel driver removal failed (exit $LASTEXITCODE)." }
