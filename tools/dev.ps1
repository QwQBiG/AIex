[CmdletBinding()]
param(
    [ValidateSet('core', 'desktop', 'native')][string]$Workspace = 'core',
    [ValidateSet('check', 'build', 'test', 'clippy', 'verify')][string]$Task = 'check',
    [ValidatePattern('^[A-Za-z0-9_-]+$')][string[]]$Package = @(),
    [ValidateRange(1, 16)][int]$Jobs = 2,
    [ValidateRange(1, 1048576)][int]$MinimumFreeGiB = 10,
    [switch]$KeepIncremental,
    [switch]$Release,
    [switch]$Offline,
    [switch]$Plan
)

$ErrorActionPreference = 'Stop'
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$manifest = switch ($Workspace) {
    'core' { Join-Path $repository 'Cargo.toml' }
    'desktop' { Join-Path $repository 'crates/ai-ex-desktop/Cargo.toml' }
    'native' { Join-Path $repository 'native-workers/Cargo.toml' }
}
$target = if ($Workspace -eq 'desktop') {
    Join-Path $repository 'crates/ai-ex-desktop/target'
} else {
    Join-Path $repository 'target'
}
$drive = [IO.Path]::GetPathRoot($target).TrimEnd('\').TrimEnd(':')
$free = (Get-PSDrive -Name $drive).Free
$required = [long]$MinimumFreeGiB * 1GB
if ($free -lt $required) {
    throw "Insufficient free disk space: $free bytes available, $required required. Inspect tools/manage_build_cache.ps1 first."
}
$incremental = if ($KeepIncremental) { '1' } elseif ($Task -in @('test', 'clippy', 'verify')) { '0' } else { $env:CARGO_INCREMENTAL }
if (-not $incremental) { $incremental = '1' }
$base = @('--manifest-path', $manifest, '--all-features', '--locked')
if ($Package.Count) {
    foreach ($name in $Package) { $base += @('--package', $name) }
} else {
    $base += '--workspace'
}
if ($Release) { $base += '--release' }
if ($Offline) { $base += '--offline' }
$steps = if ($Task -eq 'verify') { @('fmt', 'test', 'clippy') } else { @($Task) }
$commands = foreach ($step in $steps) {
    $arguments = if ($step -eq 'fmt') {
        @('fmt', '--manifest-path', $manifest, '--all', '--', '--check')
    } else {
        @($step) + $base + $(if ($step -eq 'clippy') { @('--all-targets', '--', '-D', 'warnings') } else { @() })
    }
    [pscustomobject]@{ program = 'cargo'; arguments = $arguments }
}
if ($Plan) {
    [ordered]@{
        workspace = $Workspace; target = $target; free_bytes = $free; minimum_free_bytes = $required
        incremental = $incremental; jobs = $Jobs; commands = @($commands)
    } | ConvertTo-Json -Depth 6
    exit 0
}
$names = @('CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location -LiteralPath $repository
try {
    $env:CARGO_TARGET_DIR = $target
    $env:CARGO_INCREMENTAL = $incremental
    $env:CARGO_BUILD_JOBS = [string]$Jobs
    foreach ($command in $commands) {
        $arguments = $command.arguments
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
} finally {
    Pop-Location
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
