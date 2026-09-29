#Requires -Version 7.0
# Builds only. Never installs drivers or changes signing/boot configuration.
[CmdletBinding()]
param([ValidateSet('x64','ARM64')][string]$Architecture='x64', [string]$MicrosoftSourceRoot='')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$root=$PSScriptRoot
if (!$IsWindows) { throw 'Build the native driver on Windows (GitHub Actions: windows-2022).' }
& (Join-Path $root 'setup-wdk.ps1') -Architecture $Architecture -PackagesDirectory $env:BABEL_WDK_PACKAGES
$target=if ($Architecture -eq 'ARM64') {'aarch64-pc-windows-msvc'} else {'x86_64-pc-windows-msvc'}
$build=Join-Path $root ('build\'+$Architecture+'-'+[Guid]::NewGuid().ToString('N'))
$source=Join-Path $build 'wdk'
$rustTarget=Join-Path $build 'rust'
New-Item -ItemType Directory -Path $build | Out-Null
foreach ($tool in @('cargo','rustup','python')) { if (!(Get-Command $tool -ErrorAction SilentlyContinue)) { throw "Missing $tool. Install Python and Rust before building." } }
$argsPrepare=@((Join-Path $root 'prepare.py'),'--output',$source,'--wdk-packages',$env:BABEL_WDK_PACKAGES)
if ($MicrosoftSourceRoot) { $argsPrepare+=@('--source-root',$MicrosoftSourceRoot) }
& python @argsPrepare
if ($LASTEXITCODE) { throw 'Pinned WDK source preparation failed.' }
& rustup target add $target
if ($LASTEXITCODE) { throw 'Rust target setup failed.' }
$previousFlags=$env:RUSTFLAGS
$previousIncremental=$env:CARGO_INCREMENTAL
try {
    $env:CARGO_INCREMENTAL='0'
    # Atomic and volatile integer transfers do not use SIMD or floating point.
    $env:RUSTFLAGS='-C no-redzone=yes -C target-feature=+crt-static -C panic=abort'
    & cargo build --locked --manifest-path (Join-Path $root 'transport\Cargo.toml') --target $target --target-dir $rustTarget --release --features kernel
    if ($LASTEXITCODE) { throw 'Rust kernel transport build failed.' }
} finally { $env:RUSTFLAGS=$previousFlags; $env:CARGO_INCREMENTAL=$previousIncremental }
$library=Join-Path $rustTarget "$target\release\babel_windows_transport.lib"
if (!(Test-Path $library)) { throw "Missing Rust static library: $library" }
$log=Join-Path $build 'wdk-build.binlog'
& $env:BABEL_MSBUILD (Join-Path $source 'SimpleAudioSample.sln') /m /t:Build "/p:Configuration=Release" "/p:Platform=$Architecture" "/p:BabelTransportLib=$library" /p:WindowsTargetPlatformVersion=10.0.26100.0 /p:KmdfVersion=1.31 /p:_NT_TARGET_VERSION=0xA000008 /p:SignMode=Off /p:EnableInf2cat=true /p:Inf2CatUseLocalTime=true "/bl:$log"
if ($LASTEXITCODE) { throw 'WDK build/INF validation failed. No package will be published.' }
try {
    $env:RUSTFLAGS='-C target-feature=+crt-static'
    & cargo build --locked --manifest-path (Join-Path $root 'installer\Cargo.toml') --target $target --target-dir $rustTarget --release
    if ($LASTEXITCODE) { throw 'Rust installer build failed.' }
} finally { $env:RUSTFLAGS=$previousFlags }
$packages=@(Get-ChildItem $source -Filter BabelAudio.cat -Recurse | Where-Object { (Test-Path (Join-Path $_.DirectoryName 'BabelAudio.inf')) -and (Test-Path (Join-Path $_.DirectoryName 'BabelAudio.sys')) })
if ($packages.Count -ne 1) { throw "Expected one complete WDK package, found $($packages.Count). Inspect $build" }
$dist=Join-Path $root "dist\$Architecture"
if (Test-Path $dist) { throw "Package already exists at $dist; archive it before publishing another build." }
New-Item -ItemType Directory -Path $dist | Out-Null
foreach ($name in @('BabelAudio.inf','BabelAudio.sys','BabelAudio.cat')) { Copy-Item (Join-Path $packages[0].DirectoryName $name) $dist }
Copy-Item (Join-Path $rustTarget "$target\release\babel-driver-installer.exe") $dist
foreach ($name in @('install.ps1','uninstall.ps1','LICENSE-Microsoft.txt','LICENSE-MIT.txt','README.md')) { Copy-Item (Join-Path $root $name) $dist }
Copy-Item (Join-Path $env:BABEL_WDK_PACKAGES ('toolchain-'+$Architecture+'.json')) (Join-Path $dist 'toolchain.json')
Get-ChildItem $dist -File | Sort-Object Name | ForEach-Object {
    (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()+'  '+$_.Name
} | Set-Content (Join-Path $dist 'SHA256SUMS') -Encoding utf8NoBOM
if ($env:GITHUB_OUTPUT) {
    Add-Content $env:GITHUB_OUTPUT ('package_path='+$dist) -Encoding utf8NoBOM
    Add-Content $env:GITHUB_OUTPUT ('build_path='+$build) -Encoding utf8NoBOM
}
Write-Host "Unsigned build package: $dist"
Write-Host 'Obtain the appropriate Microsoft/test signature before running install.ps1. No driver was installed.'
