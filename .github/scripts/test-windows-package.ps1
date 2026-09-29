#Requires -Version 7.0
# Only for the disposable GitHub runner. Never starts audio or installs a driver.
[CmdletBinding()]
param([string]$PackageDirectory='artifacts/installers')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or !$env:RUNNER_TEMP -or !$IsWindows) {
    throw 'Installer smoke runs only in an ephemeral Windows GitHub Actions runner.'
}
$packages=@(Get-ChildItem -LiteralPath $PackageDirectory -Filter '*-windows-x64-development.exe' -File)
if ($packages.Count -ne 1) { throw 'Expected exactly one x64 installer.' }
$manifestPath=Join-Path $packages[0].DirectoryName ($packages[0].BaseName+'-manifest.json')
$manifest=Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$destination=Join-Path $env:RUNNER_TEMP ('Babel installer smoke '+[Guid]::NewGuid().ToString('N'))
$log=Join-Path $env:RUNNER_TEMP 'babel-installer-smoke.log'
$uninstallLog=Join-Path $env:RUNNER_TEMP 'babel-uninstaller-smoke.log'
function Invoke-Wait([string]$File, [string[]]$Arguments) {
    $process=Start-Process -FilePath $File -ArgumentList $Arguments -PassThru
    if (!$process.WaitForExit(180000)) {
        $process.Kill()
        throw "Installer process timed out: $File"
    }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Installer process failed ($($process.ExitCode)): $File" }
}
try {
    Invoke-Wait $packages[0].FullName @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/SP-','/NOICONS',('/DIR="'+$destination+'"'),('/LOG="'+$log+'"'))
    $prefix=[IO.Path]::GetFullPath($destination)+[IO.Path]::DirectorySeparatorChar
    foreach ($entry in $manifest.files.PSObject.Properties) {
        $file=[IO.Path]::GetFullPath((Join-Path $destination $entry.Name))
        if (!$file.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { throw 'Manifest path escapes the package.' }
        if (!(Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing installed payload: $($entry.Name)" }
        if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $entry.Value) { throw "Installed payload checksum mismatch: $($entry.Name)" }
    }
    & (Join-Path $destination 'babel.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed application could not execute --version.' }
    & (Join-Path $destination 'drivers\windows\babel-driver-installer.exe') check-absent
    if ($LASTEXITCODE -ne 0) { throw 'Installer unexpectedly created a driver device.' }
} finally {
    $uninstaller=Join-Path $destination 'unins000.exe'
    if (Test-Path -LiteralPath $uninstaller) {
        Invoke-Wait $uninstaller @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',('/LOG="'+$uninstallLog+'"'))
    }
}
foreach ($name in @('babel.exe','babel-tray.exe','drivers\windows\babel-driver-installer.exe')) {
    if (Test-Path -LiteralPath (Join-Path $destination $name)) { throw "Uninstall left a program payload: $name" }
}
Write-Host 'Actual x64 installer payload, CLI startup, absent driver and uninstall verified.'
