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

function Invoke-PackageCheck([string]$Executable, [string]$Argument) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.ArgumentList.Add($Argument)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.Encoding]::UTF8
    $info.StandardErrorEncoding = [Text.Encoding]::UTF8
    $process = [Diagnostics.Process]::Start($info)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw "Package check timed out: $Executable"
        }
        if ($process.ExitCode -ne 0) {
            throw "Package check failed ($($process.ExitCode)): $($errors.GetAwaiter().GetResult())"
        }
        return $output.GetAwaiter().GetResult()
    } finally { $process.Dispose() }
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

# Match the development entry point even when the caller has a target override.
$coreTarget = Join-Path $repository 'target'
$desktopTarget = Join-Path $repository 'crates/ai-ex-desktop/target'
$binaries = @{
    'AIex.exe' = Join-Path $desktopTarget 'release/ai-ex-desktop.exe'
    'ai-ex-service.exe' = Join-Path $coreTarget 'release/ai-ex-service.exe'
    'ai-ex-migrate.exe' = Join-Path $coreTarget 'release/ai-ex-migrate.exe'
    'ai-ex-platform-helper.exe' = Join-Path $coreTarget 'release/ai-ex-platform-helper.exe'
}
foreach ($path in $binaries.Values) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Release executable missing: $path" }
    $stream = [IO.File]::OpenRead($path)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        $stream.Position = 0x3c
        $header = $reader.ReadInt32()
        $stream.Position = $header
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw "Expected a Windows x64 executable: $path"
        }
        if ([IO.Path]::GetFileName($path) -eq 'ai-ex-desktop.exe') {
            $stream.Position = $header + 24 + 68
            if ($reader.ReadUInt16() -ne 2) { throw 'Desktop must use the Windows GUI subsystem.' }
        }
    } finally { $reader.Dispose() }
}

# Explicit allowlist: never copy local settings, credentials, memory or logs.
[IO.Directory]::CreateDirectory((Join-Path $destination 'data')) | Out-Null
foreach ($entry in $binaries.GetEnumerator()) {
    Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $destination $entry.Key)
}
Copy-Item -LiteralPath (Join-Path $repository 'config/ai-ex.portable.example.toml') -Destination (Join-Path $destination 'data/ai-ex.local.toml')
[IO.File]::WriteAllText((Join-Path $destination 'AIex.portable'), "1`n", [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $repository 'docs/PORTABLE_START.txt') -Destination (Join-Path $destination '开始使用.txt')
Copy-Item -LiteralPath (Join-Path $repository 'LICENSE') -Destination (Join-Path $destination 'LICENSE.txt')
Copy-Item -LiteralPath (Join-Path $repository 'docs/PORTABLE_THIRD_PARTY.txt') -Destination (Join-Path $destination 'THIRD-PARTY-LICENSES.txt')
$testWindow = "@echo off`r`nsetlocal`r`nstart `"`" `"%~dp0ai-ex-platform-helper.exe`" --test-window`r`n"
[IO.File]::WriteAllText((Join-Path $destination 'AIex-Test-Window.cmd'), $testWindow, [Text.UTF8Encoding]::new($false))

$check = Invoke-PackageCheck (Join-Path $destination 'AIex.exe') '--check-install'
$status = $check | ConvertFrom-Json
if (-not $status.ready -or $status.version -ne $version) { throw 'Packaged desktop version check failed.' }
$serviceVersion = Invoke-PackageCheck (Join-Path $destination 'ai-ex-service.exe') '--version'
if ($serviceVersion.Trim() -ne "ai-ex-service $version") {
    throw 'Packaged service version check failed.'
}
$migratorVersion = Invoke-PackageCheck (Join-Path $destination 'ai-ex-migrate.exe') '--version'
if ($migratorVersion.Trim() -ne "ai-ex-migrate $version") {
    throw 'Packaged migrator version check failed.'
}
$helperVersion = Invoke-PackageCheck (Join-Path $destination 'ai-ex-platform-helper.exe') '--version'
if ($helperVersion.Trim() -ne "ai-ex-platform-helper $version") { throw 'Packaged platform helper version check failed.' }

Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory($destination, $archive, [IO.Compression.CompressionLevel]::Optimal, $true)
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText($checksum, "$hash  $name.zip`n", [Text.UTF8Encoding]::new($false))
[pscustomobject]@{ version = $version; directory = $destination; archive = $archive; sha256 = $hash; ready = $true } | ConvertTo-Json
