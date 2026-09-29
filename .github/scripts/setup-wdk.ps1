#Requires -Version 7.0
[CmdletBinding()]
param([ValidateSet('x64','ARM64')][string]$Architecture='x64')
$ErrorActionPreference='Stop'
$repo=Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
& (Join-Path $repo 'native\windows\setup-wdk.ps1') -Architecture $Architecture -GitHubEnvironment
