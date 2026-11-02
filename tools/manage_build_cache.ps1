[CmdletBinding()]
param([switch]$Apply)

$ErrorActionPreference = 'Stop'
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$relativeTargets = @(
    'target/debug/incremental',
    'crates/ai-ex-desktop/target/debug/incremental',
    'target/m0-fetch-native-20261029/target/debug/incremental',
    'target/native-ui-checks-20261101/debug/incremental',
    'target/native-model-manager-20261101/debug/incremental'
)
$allowedExtensions = @('.lock', '.bin', '.rmeta', '.o')

function Find-ActiveBuild {
    @(Get-CimInstance Win32_Process | Where-Object {
        $_.Name -in @('cargo.exe', 'rustc.exe', 'clippy-driver.exe') -and
        $_.CommandLine -notmatch '\bfmt\b'
    })
}

function Inspect-Cache([string]$Relative) {
    $path = [IO.Path]::GetFullPath((Join-Path $repository $Relative))
    if (-not $path.StartsWith($repository + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Cache target is outside the repository.'
    }
    $tracked = @(git -C $repository ls-files -- $Relative)
    if ($LASTEXITCODE -ne 0 -or $tracked.Count) { throw 'Cache target contains tracked files.' }
    git -C $repository check-ignore --quiet -- $Relative
    if ($LASTEXITCODE -ne 0) { throw 'Cache target must be ignored.' }
    $bytes = [long]0
    $count = 0
    if (Test-Path -LiteralPath $path) {
        $directory = Get-Item -LiteralPath $path -Force
        $ancestor = $directory
        while ($ancestor -and $ancestor.FullName.StartsWith($repository, [StringComparison]::OrdinalIgnoreCase)) {
            if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cache ancestry contains a link.' }
            $ancestor = $ancestor.Parent
        }
        $pending = [Collections.Generic.Stack[IO.DirectoryInfo]]::new()
        $pending.Push($directory)
        while ($pending.Count) {
            foreach ($entry in $pending.Pop().EnumerateFileSystemInfos()) {
                if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cache contains a link.' }
                if ($entry.Attributes -band [IO.FileAttributes]::Directory) {
                    $pending.Push([IO.DirectoryInfo]$entry)
                } else {
                    if ($entry.Extension -notin $allowedExtensions) { throw "Unexpected cache file: $($entry.FullName)" }
                    if ($entry.Attributes -band [IO.FileAttributes]::ReadOnly) { throw 'Read-only cache file requires manual review.' }
                    if ($Apply -and $entry.Extension -eq '.lock') {
                        $lease = [IO.File]::Open($entry.FullName, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
                        $lease.Dispose()
                    }
                    $bytes += $entry.Length
                    $count++
                }
            }
        }
    }
    [pscustomobject]@{ path = $path; relative = $Relative; exists = (Test-Path -LiteralPath $path); bytes = $bytes; files = $count }
}

$builds = Find-ActiveBuild
if ($Apply -and $builds.Count) { throw 'Stop all Cargo builds before cache cleanup.' }
$drive = [IO.Path]::GetPathRoot($repository).TrimEnd('\').TrimEnd(':')
$before = (Get-PSDrive -Name $drive).Free
# Validate every target before removing any directory.
$targets = @($relativeTargets | ForEach-Object { Inspect-Cache $_ })
if ($Apply) {
    foreach ($target in $targets | Where-Object exists) {
        if ((Find-ActiveBuild).Count) { throw 'A Cargo build started; remaining caches were preserved.' }
        Remove-Item -LiteralPath $target.path -Recurse
        if (Test-Path -LiteralPath $target.path) { throw 'Cache directory remains after cleanup.' }
    }
}
$after = (Get-PSDrive -Name $drive).Free
[ordered]@{
    mode = $(if ($Apply) { 'cleaned' } else { 'inspect_only' })
    active_builds = $builds.Count
    logical_cache_bytes = ($targets | Measure-Object bytes -Sum).Sum
    before_free_bytes = $before
    after_free_bytes = $after
    free_increase_bytes = $after - $before
    targets = $targets
} | ConvertTo-Json -Depth 6
