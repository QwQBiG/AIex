function Get-ArchitectureManifestPath
{
    param([Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Path)
    return [IO.Path]::GetFullPath($Path)
}

function Get-ArchitectureViolations
{
    param(
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$WorkspaceRoot,
        [Parameter(Mandatory)][object[]]$Metadata,
        [Parameter(Mandatory)][hashtable]$Allowed
    )

    $ErrorActionPreference = 'Stop'
    $pathComparer = [StringComparer]::Ordinal
    if ($IsWindows) { $pathComparer = [StringComparer]::OrdinalIgnoreCase }
    $members = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $packages = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    $manifests = [Collections.Generic.HashSet[string]]::new($pathComparer)
    $violations = [Collections.Generic.List[object]]::new()
    foreach ($document in $Metadata)
    {
        foreach ($id in $document.workspace_members) { [void]$members.Add([string]$id) }
    }
    foreach ($document in $Metadata)
    {
        foreach ($package in $document.packages)
        {
            if (!$members.Contains([string]$package.id)) { continue }
            $name = [string]$package.name
            $manifest = Get-ArchitectureManifestPath $package.manifest_path
            if ($packages.ContainsKey($name))
            {
                $violations.Add([pscustomobject]@{
                    package = $name; dependency = '<duplicate package name>'; manifest = $manifest
                })
                continue
            }
            $packages.Add($name, $package)
            [void]$manifests.Add($manifest)
            if (!$Allowed.ContainsKey($name))
            {
                $violations.Add([pscustomobject]@{
                    package = $name; dependency = '<missing architecture policy>'
                })
            }
        }
    }

    foreach ($package in $packages.Values)
    {
        foreach ($dependency in $package.dependencies)
        {
            $name = [string]$dependency.name
            $internal = $packages.ContainsKey($name)
            if ($dependency.path)
            {
                $manifest = Get-ArchitectureManifestPath (Join-Path $dependency.path 'Cargo.toml')
                if (!$internal)
                {
                    $violations.Add([pscustomobject]@{
                        package = $package.name; dependency = $name
                        reason = 'Local dependency is not a registered workspace package'; manifest = $manifest
                    })
                }
                elseif (!$pathComparer.Equals($manifest, (Get-ArchitectureManifestPath $packages[$name].manifest_path)))
                {
                    $violations.Add([pscustomobject]@{
                        package = $package.name; dependency = $name
                        reason = 'Local dependency does not use the registered manifest'; manifest = $manifest
                    })
                }
            }
            elseif ($internal)
            {
                $violations.Add([pscustomobject]@{
                    package = $package.name; dependency = $name
                    reason = 'Internal dependency must use the registered local path'
                })
            }
            if ($internal -and $name -cnotin $Allowed[$package.name])
            {
                $violations.Add([pscustomobject]@{ package = $package.name; dependency = $name })
            }
        }
    }

    # Package directories are direct children of crates; build output is not a package inventory.
    $cratesRoot = Join-Path $WorkspaceRoot 'crates'
    foreach ($directory in Get-ChildItem -LiteralPath $cratesRoot -Directory -Force)
    {
        $manifest = Join-Path $directory.FullName 'Cargo.toml'
        if ((Test-Path -LiteralPath $manifest -PathType Leaf) -and
            !$manifests.Contains((Get-ArchitectureManifestPath $manifest)))
        {
            $violations.Add([pscustomobject]@{
                package = $directory.Name; dependency = '<unregistered crate manifest>'; manifest = $manifest
            })
        }
    }
    return $violations.ToArray()
}
