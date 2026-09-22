param(
    [switch]$ProbeViolation
)

$ErrorActionPreference = 'Stop'
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

$allowed = @{
    'ai-ex-domain' = @()
    'ai-ex-protocol' = @('ai-ex-domain')
    'ai-ex-event-bus' = @('ai-ex-domain', 'ai-ex-protocol')
    'ai-ex-stage' = @('ai-ex-domain')
    'ai-ex-stage-obs' = @('ai-ex-domain', 'ai-ex-stage')
    'ai-ex-bilibili' = @('ai-ex-domain', 'ai-ex-event-bus')
    'ai-ex-plugin' = @('ai-ex-domain')
    'ai-ex-simulator' = @('ai-ex-domain', 'ai-ex-event-bus', 'ai-ex-memory')
    'ai-ex-config' = @('ai-ex-domain')
    'ai-ex-text' = @('ai-ex-domain')
    'ai-ex-core' = @('ai-ex-domain', 'ai-ex-text', 'ai-ex-protocol', 'ai-ex-stage')
    'ai-ex-duplex' = @('ai-ex-domain')
    'ai-ex-asr' = @('ai-ex-domain', 'ai-ex-duplex')
    'ai-ex-capture' = @('ai-ex-domain', 'ai-ex-duplex')
    'ai-ex-audio' = @('ai-ex-core', 'ai-ex-domain', 'ai-ex-text', 'ai-ex-stage')
    'ai-ex-memory' = @('ai-ex-core', 'ai-ex-domain')
    'ai-ex-deepseek' = @('ai-ex-core', 'ai-ex-domain')
    'ai-ex-ollama' = @('ai-ex-core', 'ai-ex-domain')
    'ai-ex-koboldcpp' = @('ai-ex-core', 'ai-ex-domain')
    'ai-ex-tts' = @('ai-ex-domain')
    'ai-ex-vts' = @('ai-ex-core', 'ai-ex-domain', 'ai-ex-stage')
    'ai-ex-observability' = @('ai-ex-core', 'ai-ex-domain')
    'ai-ex-safety' = @('ai-ex-domain')
    'ai-ex-automation' = @('ai-ex-domain', 'ai-ex-safety')
    'ai-ex-audit' = @('ai-ex-automation', 'ai-ex-domain', 'ai-ex-safety')
    'ai-ex-vision' = @('ai-ex-domain')
    'ai-ex-control' = @('ai-ex-domain', 'ai-ex-observability')
    'ai-ex-ui-model' = @('ai-ex-domain', 'ai-ex-observability')
    'ai-ex-migrate' = @('ai-ex-config', 'ai-ex-domain')
    'ai-ex-desktop' = @('ai-ex-config', 'ai-ex-control', 'ai-ex-domain', 'ai-ex-observability', 'ai-ex-ui-model')
    'ai-ex-service' = @(
        'ai-ex-asr', 'ai-ex-audit', 'ai-ex-audio', 'ai-ex-automation', 'ai-ex-bilibili', 'ai-ex-capture',
        'ai-ex-config', 'ai-ex-control', 'ai-ex-core', 'ai-ex-domain', 'ai-ex-event-bus',
        'ai-ex-deepseek',
        'ai-ex-duplex', 'ai-ex-koboldcpp', 'ai-ex-memory', 'ai-ex-observability',
        'ai-ex-ollama',
        'ai-ex-plugin',
        'ai-ex-safety', 'ai-ex-stage', 'ai-ex-stage-obs', 'ai-ex-tts', 'ai-ex-vision', 'ai-ex-vts'
    )
}

$workspaceRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'architecture/validation.ps1')
$metadata = cargo metadata --manifest-path (Join-Path $workspaceRoot 'Cargo.toml') --no-deps --locked --offline --format-version 1
if ($LASTEXITCODE -ne 0)
{
    throw 'cargo metadata failed'
}
$metadata = $metadata | ConvertFrom-Json
$desktopMetadata = cargo metadata --manifest-path (Join-Path $workspaceRoot 'crates/ai-ex-desktop/Cargo.toml') --no-deps --locked --offline --format-version 1
if ($LASTEXITCODE -ne 0)
{
    throw 'desktop cargo metadata failed'
}
$desktopMetadata = $desktopMetadata | ConvertFrom-Json
$packages = @($metadata.packages) + @($desktopMetadata.packages)
$workspaceIds = [Collections.Generic.HashSet[string]]::new(
    [string[]](@($metadata.workspace_members) + @($desktopMetadata.workspace_members))
)
if ($ProbeViolation)
{
    $service = $packages | Where-Object name -EQ 'ai-ex-service' | Select-Object -First 1
    foreach ($package in $packages)
    {
        if ($package.name -in @('ai-ex-domain', 'ai-ex-desktop'))
        {
            $package.dependencies = @($package.dependencies) + @([pscustomobject]@{
                name = 'ai-ex-service'; path = Split-Path -Parent $service.manifest_path
            })
        }
    }
}
$violations = @(Get-ArchitectureViolations -WorkspaceRoot $workspaceRoot `
    -Metadata @($metadata, $desktopMetadata) -Allowed $allowed)

if ($violations.Count -gt 0)
{
    [pscustomobject]@{
        status = 'failed'
        violations = $violations
    } | ConvertTo-Json -Depth 4
    exit 1
}

[pscustomobject]@{
    status = 'passed'
    checked_packages = $workspaceIds.Count
    workspace_packages = @($metadata.workspace_members).Count
    desktop_packages = @($desktopMetadata.workspace_members).Count
} | ConvertTo-Json -Compress
