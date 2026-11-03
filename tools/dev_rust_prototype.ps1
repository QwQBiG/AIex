[CmdletBinding()]
param(
    [ValidateSet('core', 'desktop', 'native')][string]$Workspace = 'core',
    [ValidateSet('check', 'build', 'test', 'clippy', 'verify')][string]$Task = 'check',
    [ValidatePattern('^[A-Za-z0-9_-]+$')][string[]]$Package = @(),
    [string]$TestFilter,
    [ValidateRange(1, 16)][int]$Jobs = 2,
    [ValidateRange(1, 1048576)][int]$MinimumFreeGiB = 10,
    [switch]$KeepIncremental,
    [switch]$Release,
    [switch]$Offline,
    [switch]$Plan
)

$ErrorActionPreference = 'Stop'
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$arguments = @{}
foreach ($entry in $PSBoundParameters.GetEnumerator()) { $arguments[$entry.Key] = $entry.Value }
$arguments['Plan'] = $true
$global:LASTEXITCODE = 0
$preview = @(& (Join-Path $PSScriptRoot 'dev.ps1') @arguments)
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Plan) { $preview; exit 0 }

if ($env:CARGO_BUILD_TARGET) {
    throw 'The prototype requires the default build target for its bootstrap executable.'
}

# The existing plan validates options and the selected workspace's free space.
# Bootstrapping also writes to the root target, so check that volume explicitly.
$target = Join-Path $repository 'target'
if ((Test-Path -LiteralPath $target) -and
    ((Get-Item -LiteralPath $target).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'The prototype cannot bootstrap through a linked target directory.'
}
$drive = [IO.Path]::GetPathRoot($target).TrimEnd('\').TrimEnd(':')
$free = (Get-PSDrive -Name $drive).Free
$required = [long]$MinimumFreeGiB * 1GB
if ($free -lt $required) {
    throw "Insufficient free disk space: $free bytes available, $required required. Inspect tools/manage_build_cache.ps1 first."
}
[void][IO.Directory]::CreateDirectory($target)
$leasePath = Join-Path $target '.development.lock'
try {
    $lease = [IO.File]::Open($leasePath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
} catch [IO.IOException] {
    if ($Workspace -eq 'desktop') {
        # Desktop has its own target lease; keep it usable during a core build.
        $arguments.Remove('Plan')
        & (Join-Path $PSScriptRoot 'dev.ps1') @arguments
        exit $LASTEXITCODE
    }
    throw "Development target is already in use or inaccessible: $target. Wait for the current task to finish before starting another."
}
$temporary = Join-Path $target ('xtask-dev-' + [guid]::NewGuid().ToString('N') + '.exe')
$ownedTemporary = $false
$previous = @{}
foreach ($name in @('CARGO_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS')) {
    $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
try {
    $pushed = $false
    try {
        Push-Location -LiteralPath $repository
        $pushed = $true
        $env:CARGO_TARGET_DIR = $target
        $env:CARGO_INCREMENTAL = '0'
        $env:CARGO_BUILD_JOBS = [string]$Jobs
        $build = @('build', '--manifest-path', (Join-Path $repository 'Cargo.toml'), '--package', 'xtask', '--all-features', '--locked')
        if ($Offline) { $build += '--offline' }
        & cargo @build
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $source = [IO.File]::OpenRead((Join-Path $target 'debug/xtask.exe'))
        try {
            $destination = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            $ownedTemporary = $true
            try { $source.CopyTo($destination) } finally { $destination.Dispose() }
        } finally { $source.Dispose() }
    } finally {
        if ($pushed) { Pop-Location }
        foreach ($name in $previous.Keys) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
        $lease.Dispose()
    }

    $run = @('dev', '--project', $repository, '--workspace', $Workspace, '--task', $Task, '--jobs', [string]$Jobs, '--minimum-free-gib', [string]$MinimumFreeGiB)
    foreach ($name in $Package) { $run += @('--package', $name) }
    if ($PSBoundParameters.ContainsKey('TestFilter')) { $run += @('--test-filter', $TestFilter) }
    if ($KeepIncremental) { $run += '--keep-incremental' }
    if ($Release) { $run += '--release' }
    if ($Offline) { $run += '--offline' }
    & $temporary @run
    exit $LASTEXITCODE
} finally {
    if ($ownedTemporary) { Remove-Item -LiteralPath $temporary }
}
