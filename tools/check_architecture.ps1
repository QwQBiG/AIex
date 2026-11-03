param(
    [switch]$ProbeViolation
)

$ErrorActionPreference = 'Stop'
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$arguments = @('run', '--manifest-path', (Join-Path $workspaceRoot 'Cargo.toml'), '--package', 'xtask', '--locked', '--offline', '--', 'architecture')
if ($ProbeViolation) { $arguments += '--probe-violation' }
& cargo @arguments
exit $LASTEXITCODE
