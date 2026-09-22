$ErrorActionPreference = 'Stop'
$OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
. (Join-Path $PSScriptRoot 'architecture/validation.ps1')

$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('architecture-fixture-' + [Guid]::NewGuid().ToString('N'))
if (Test-Path -LiteralPath $fixtureRoot) { throw 'Fixture directory already exists' }
$results = [Collections.Generic.List[object]]::new()
try
{
    $packages = @()
    foreach ($name in @('ai-ex-domain', 'ai-ex-adapter', 'ai-ex-desktop'))
    {
        $directory = Join-Path $fixtureRoot "crates/$name"
        New-Item -ItemType Directory -Path $directory | Out-Null
        $manifest = Join-Path $directory 'Cargo.toml'
        @("[package]", "name = `"$name`"", 'version = "0.1.0"', '[lib]', 'path = "lib.rs"') |
            Set-Content -LiteralPath $manifest -Encoding UTF8
        '' | Set-Content -LiteralPath (Join-Path $directory 'lib.rs') -Encoding UTF8
        $packages += [pscustomobject]@{
            id = $name; name = $name; manifest_path = $manifest; dependencies = @()
        }
    }
    $domainPath = Split-Path -Parent $packages[0].manifest_path
    $adapterPath = Split-Path -Parent $packages[1].manifest_path
    foreach ($package in $packages[1..2])
    {
        $package.dependencies = @([pscustomobject]@{
            name = 'ai-ex-domain'; path = $domainPath; rename = $null
            kind = $null; target = $null; optional = $false
        })
    }
    $baseline = @(
        [pscustomobject]@{ workspace_members = @('ai-ex-domain', 'ai-ex-adapter'); packages = $packages[0..1] },
        [pscustomobject]@{ workspace_members = @('ai-ex-desktop'); packages = @($packages[2]) }
    ) | ConvertTo-Json -Depth 20
    # A build directory may contain manifests; it is outside the source package inventory.
    $buildDirectory = Join-Path $adapterPath 'target/generated'
    New-Item -ItemType Directory -Path $buildDirectory | Out-Null
    '' | Set-Content -LiteralPath (Join-Path $buildDirectory 'Cargo.toml') -Encoding UTF8

    $cases = @(
        @{ name = 'valid-independent-workspaces'; count = 0; mutate = {} },
        @{ name = 'renamed-local-dependency'; count = 0; mutate = {
            param($metadata) $metadata[1].packages[0].dependencies[0].rename = 'foundation'
        } },
        @{ name = 'normalized-local-path'; count = 0; mutate = {
            param($metadata) $metadata[1].packages[0].dependencies[0].path = Join-Path $domainPath '../ai-ex-domain'
        } },
        @{ name = 'platform-path-case'; count = $(if ($IsWindows) { 0 } else { 1 }); mutate = {
            param($metadata) $metadata[1].packages[0].dependencies[0].path = $domainPath.ToUpperInvariant()
        } },
        @{ name = 'unregistered-local-dependency'; count = 1; message = 'not a registered workspace package'; mutate = {
            param($metadata)
            $dependency = $metadata[1].packages[0].dependencies[0]
            $dependency.name = 'ai-ex-private'
            $dependency.path = Join-Path $fixtureRoot 'private'
        } },
        @{ name = 'same-name-wrong-manifest'; count = 1; message = 'does not use the registered manifest'; mutate = {
            param($metadata) $metadata[1].packages[0].dependencies[0].path = $adapterPath
        } },
        @{ name = 'same-name-registry-dependency'; count = 1; message = 'must use the registered local path'; mutate = {
            param($metadata) $metadata[1].packages[0].dependencies[0].path = $null
        } },
        @{ name = 'forbidden-normal-dependency'; count = 1; message = 'ai-ex-adapter'; mutate = {
            param($metadata)
            $metadata[0].packages[0].dependencies = @([pscustomobject]@{
                name = 'ai-ex-adapter'; path = $adapterPath; kind = $null; target = $null; optional = $false
            })
        } },
        @{ name = 'forbidden-dev-dependency'; count = 1; message = 'ai-ex-adapter'; mutate = {
            param($metadata)
            $metadata[0].packages[0].dependencies = @([pscustomobject]@{
                name = 'ai-ex-adapter'; path = $adapterPath; kind = 'dev'; target = $null; optional = $false
            })
        } },
        @{ name = 'forbidden-target-build-dependency'; count = 1; message = 'ai-ex-adapter'; mutate = {
            param($metadata)
            $metadata[0].packages[0].dependencies = @([pscustomobject]@{
                name = 'ai-ex-adapter'; path = $adapterPath; kind = 'build'; target = 'cfg(windows)'; optional = $true
            })
        } },
        @{ name = 'missing-package-policy'; count = 1; message = '<missing architecture policy>'; mutate = {
            param($metadata, $policy) $policy.Remove('ai-ex-domain')
        } },
        @{ name = 'unregistered-crate-directory'; count = 1; message = '<unregistered crate manifest>'; mutate = {
            $directory = Join-Path $fixtureRoot 'crates/unregistered'
            New-Item -ItemType Directory -Path $directory | Out-Null
            '' | Set-Content -LiteralPath (Join-Path $directory 'Cargo.toml') -Encoding UTF8
        } }
    )

    foreach ($case in $cases)
    {
        $metadata = $baseline | ConvertFrom-Json
        $policy = @{
            'ai-ex-domain' = @(); 'ai-ex-adapter' = @('ai-ex-domain'); 'ai-ex-desktop' = @('ai-ex-domain')
        }
        & $case.mutate $metadata $policy
        $violations = @(Get-ArchitectureViolations -WorkspaceRoot $fixtureRoot -Metadata $metadata -Allowed $policy)
        if ($violations.Count -ne $case.count)
        {
            throw "$($case.name): expected $($case.count) violations, got $($violations.Count): $($violations | ConvertTo-Json -Compress)"
        }
        if ($case.message -and !(@($violations | ForEach-Object { "$($_.dependency) $($_.reason)" }) -match [regex]::Escape($case.message)))
        {
            throw "$($case.name): expected diagnostic '$($case.message)'"
        }
        $results.Add([pscustomobject]@{ name = $case.name; status = 'passed' })
    }
    [pscustomobject]@{ status = 'passed'; checked_cases = $results.Count; cases = $results } | ConvertTo-Json -Depth 4
}
finally
{
    $resolved = [IO.Path]::GetFullPath($fixtureRoot)
    $expected = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) (Split-Path -Leaf $fixtureRoot)))
    if ($resolved -ne $expected -or !(Split-Path -Leaf $resolved).StartsWith('architecture-fixture-'))
    {
        throw 'Refusing to remove an unexpected fixture directory'
    }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
