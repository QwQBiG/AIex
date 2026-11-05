[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [ValidateRange(1, 16)][int]$Jobs = 2,
    [ValidateRange(1, 1048576)][int]$MinimumFreeGiB = 10,
    [switch]$SkipBuild,
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
if (-not $IsWindows) { throw 'Packaging requires PowerShell 7 on Windows.' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repository 'dist' }
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)

# Bootstrap the Rust tool under the same lease as other development tasks.
$development = Join-Path $PSScriptRoot 'dev.ps1'
$toolBuild = @{ Task = 'build'; Workspace = 'core'; Package = @('xtask'); Offline = $Offline; Jobs = $Jobs; MinimumFreeGiB = $MinimumFreeGiB }
& $development @toolBuild
if ($LASTEXITCODE -ne 0) { throw 'Packaging tool build failed.' }
$tool = Join-Path $repository 'target/debug/xtask.exe'
$arguments = @('portable-build', '--project', $repository, '--output', $outputRoot, '--jobs', [string]$Jobs, '--minimum-free-gib', [string]$MinimumFreeGiB)
if ($Offline) { $arguments += '--offline' }
if ($SkipBuild) { $arguments += '--skip-build' }
& $tool @arguments
if ($LASTEXITCODE -ne 0) { throw 'Portable packaging failed.' }
