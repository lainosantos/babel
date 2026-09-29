#Requires -Version 7.0
# Restore official, hash-pinned SDK/WDK build inputs. No device installation.
[CmdletBinding()]
param(
    [ValidateSet('x64','ARM64')][string]$Architecture='x64',
    [string]$PackagesDirectory='',
    [switch]$GitHubEnvironment
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if (!$IsWindows) { throw 'WDK setup requires Windows and Visual Studio 2022. The hosted CI image is windows-2022.' }
$manifest=Get-Content (Join-Path $PSScriptRoot 'wdk-packages.json') -Raw | ConvertFrom-Json
if (!$PackagesDirectory) {
    $cacheRoot=if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { Join-Path $PSScriptRoot 'build' }
    $PackagesDirectory=Join-Path $cacheRoot ('babel-wdk-'+$manifest.version)
}
$PackagesDirectory=[IO.Path]::GetFullPath($PackagesDirectory)
New-Item -ItemType Directory -Force -Path $PackagesDirectory | Out-Null
$vswhere="${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (!(Test-Path $vswhere)) { throw 'Visual Studio Installer/vswhere is missing. Use windows-2022 or install VS2022 C++/WDK components.' }
$vs=& $vswhere -latest -version '[17.0,18.0)' -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -or !$vs) { throw 'Visual Studio 2022 C++ build tools were not found.' }
$vs=([string]$vs).Trim()
$devShell=Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll'
Import-Module $devShell
$devArchitecture=if ($Architecture -eq 'ARM64') {'arm64'} else {'amd64'}
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments "-arch=$devArchitecture -host_arch=amd64"
$vcTargets=Join-Path $vs 'MSBuild\Microsoft\VC\v170'
$toolset=Join-Path $vcTargets "Platforms\$Architecture\PlatformToolsets\WindowsKernelModeDriver10.0\Toolset.props"
if (!(Test-Path $toolset)) {
    throw 'The VS2022 WDK extension/toolset is absent. windows-2022 includes it; locally install the official Windows Driver Kit Visual Studio component. NuGet supplies headers/libs, not this VS extension.'
}
$compiler=Join-Path $env:VCToolsInstallDir "bin\Hostx64\$Architecture\cl.exe"
$spectre=Join-Path $env:VCToolsInstallDir "lib\spectre\$Architecture\libcmt.lib"
foreach ($path in @($compiler,$spectre)) { if (!(Test-Path $path)) { throw "Missing C++ target/Spectre component: $path" } }
$msbuild=Join-Path $vs 'MSBuild\Current\Bin\amd64\MSBuild.exe'
if (!(Test-Path $msbuild)) { throw "MSBuild is missing: $msbuild" }

# NuGet's dependency ranges are deliberately not resolved here: every archive,
# including SDK dependencies, is version/hash locked and extracted from nuget.org.
$wanted=@('Microsoft.Windows.WDK.x64','Microsoft.Windows.SDK.CPP','Microsoft.Windows.SDK.CPP.x64')
if ($Architecture -eq 'ARM64') { $wanted+=@('Microsoft.Windows.WDK.arm64','Microsoft.Windows.SDK.CPP.arm64') }
foreach ($package in $manifest.packages | Where-Object { $_.id -in $wanted }) {
    $name=$package.id+'.'+$manifest.version
    $archive=Join-Path $PackagesDirectory ($name+'.nupkg')
    if (!(Test-Path $archive)) {
        $id=$package.id.ToLowerInvariant()
        $uri=$manifest.source+'/'+$id+'/'+$manifest.version+'/'+$id+'.'+$manifest.version+'.nupkg'
        $partial=$archive+'.partial'
        Invoke-WebRequest -Uri $uri -OutFile $partial -MaximumRetryCount 3 -RetryIntervalSec 3
        if ((Get-FileHash $partial -Algorithm SHA256).Hash -ne $package.sha256) { throw "SHA256 mismatch: $name" }
        Move-Item $partial $archive
    }
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $package.sha256) { throw "Cached NuGet archive SHA256 mismatch: $name" }
    $destination=Join-Path $PackagesDirectory $name
    $marker=Join-Path $destination '.babel-verified-sha256'
    if (!(Test-Path $destination)) {
        [IO.Compression.ZipFile]::ExtractToDirectory($archive,$destination)
        Set-Content $marker -Value $package.sha256 -Encoding utf8NoBOM
    } elseif (!(Test-Path $marker) -or (Get-Content $marker -Raw).Trim() -ne $package.sha256) {
        throw "Incomplete/unrecognized extracted package: $destination. Use a fresh PackagesDirectory."
    }
}
$targetSuffix=if ($Architecture -eq 'ARM64') {'arm64'} else {'x64'}
$wdk=Join-Path $PackagesDirectory ('Microsoft.Windows.WDK.'+$targetSuffix+'.'+$manifest.version+'\c')
$hostWdk=Join-Path $PackagesDirectory ('Microsoft.Windows.WDK.x64.'+$manifest.version+'\c')
$sdk=Join-Path $PackagesDirectory ('Microsoft.Windows.SDK.CPP.'+$manifest.version+'\c')
foreach ($path in @(
    (Join-Path $wdk ('Include\'+$manifest.kit_version+'\km\ntddk.h')),
    (Join-Path $wdk ('Lib\'+$manifest.kit_version+'\km\'+$targetSuffix+'\portcls.lib')),
    (Join-Path $wdk ('Lib\wdf\kmdf\'+$targetSuffix+'\1.31\WdfDriverEntry.lib')),
    (Join-Path $hostWdk ('bin\'+$manifest.kit_version+'\x64\stampinf.exe')),
    (Join-Path $hostWdk ('bin\'+$manifest.kit_version+'\x86\Inf2Cat.exe')),
    (Join-Path $sdk ('Include\'+$manifest.kit_version+'\shared\ks.h'))
)) { if (!(Test-Path $path)) { throw "The locked WDK/SDK package is incomplete: $path" } }
$values=[ordered]@{
    BABEL_WDK_PACKAGES=$PackagesDirectory
    BABEL_WDK_VERSION=$manifest.version
    BABEL_WDK_ARCHITECTURE=$Architecture
    BABEL_MSBUILD=$msbuild
    BABEL_VSINSTALL=$vs
}
foreach ($entry in $values.GetEnumerator()) {
    [Environment]::SetEnvironmentVariable($entry.Key,[string]$entry.Value,'Process')
    if ($GitHubEnvironment) {
        if (!$env:GITHUB_ENV) { throw '-GitHubEnvironment requires GITHUB_ENV from GitHub Actions.' }
        if ([string]$entry.Value -match "[\r\n]") { throw 'Invalid environment value.' }
        Add-Content $env:GITHUB_ENV ($entry.Key+'='+$entry.Value) -Encoding utf8NoBOM
    }
}
$evidence=[ordered]@{architecture=$Architecture;wdk_sdk_package_version=$manifest.version;kit_version=$manifest.kit_version;visual_studio=$vs;msvc_version=$env:VCToolsVersion;runner_image=$env:ImageVersion;msbuild=$msbuild;toolset=$toolset;packages=$wanted}
$evidence | ConvertTo-Json -Depth 3 | Set-Content (Join-Path $PackagesDirectory ('toolchain-'+$Architecture+'.json')) -Encoding utf8NoBOM
Write-Host "Pinned SDK/WDK $($manifest.version) ready for ${Architecture}: $PackagesDirectory"
