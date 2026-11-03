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

function Get-PackageMetadata([string]$Manifest) {
    $metadataArgs = @('metadata', '--no-deps', '--locked', '--format-version', '1', '--manifest-path', $Manifest)
    if ($Offline) { $metadataArgs += '--offline' }
    $json = & cargo @metadataArgs
    if ($LASTEXITCODE -ne 0) { throw "Cannot read metadata: $Manifest" }
    return ($json | ConvertFrom-Json)
}

$core = Get-PackageMetadata (Join-Path $repository 'Cargo.toml')
$desktop = Get-PackageMetadata (Join-Path $repository 'crates/ai-ex-desktop/Cargo.toml')
$version = ($core.packages | Where-Object name -EQ 'ai-ex-service').version
$desktopVersion = ($desktop.packages | Where-Object name -EQ 'ai-ex-desktop').version
$migratorVersion = ($core.packages | Where-Object name -EQ 'ai-ex-migrate').version
if (-not $version -or $version -ne $desktopVersion -or $version -ne $migratorVersion) {
    throw 'Desktop, service and migrator versions must match.'
}
$name = "AIex-Windows-x64-$version"
$destination = Join-Path $outputRoot $name
$archive = "$destination.zip"
$checksum = "$archive.sha256"
foreach ($path in @($destination, $archive, $checksum)) {
    if (Test-Path -LiteralPath $path) { throw "Output already exists; nothing overwritten: $path" }
}

if (-not $SkipBuild) {
    $development = Join-Path $PSScriptRoot 'dev.ps1'
    $build = @{ Task = 'build'; Release = $true; Offline = $Offline; Jobs = $Jobs; MinimumFreeGiB = $MinimumFreeGiB }
    & $development @build -Workspace core -Package @('ai-ex-service', 'ai-ex-migrate', 'ai-ex-platform-windows')
    if ($LASTEXITCODE -ne 0) { throw 'Core executable release build failed.' }
    & $development @build -Workspace desktop
    if ($LASTEXITCODE -ne 0) { throw 'Desktop release build failed.' }
}

# Build the packaging tool under the same target lease and resource limits as
# other development tasks. SkipBuild still skips rebuilding product binaries.
$development = Join-Path $PSScriptRoot 'dev.ps1'
$toolBuild = @{ Task = 'build'; Workspace = 'core'; Package = @('xtask'); Offline = $Offline; Jobs = $Jobs; MinimumFreeGiB = $MinimumFreeGiB }
& $development @toolBuild
if ($LASTEXITCODE -ne 0) { throw 'Packaging tool build failed.' }
$tool = Join-Path $repository 'target/debug/xtask.exe'
$arguments = @('portable', '--project', $repository, '--output', $outputRoot)
if ($Offline) { $arguments += '--offline' }
& $tool @arguments
if ($LASTEXITCODE -ne 0) { throw 'Portable packaging failed.' }
