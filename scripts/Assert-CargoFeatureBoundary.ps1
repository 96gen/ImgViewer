#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$ManifestPath,
    [string]$CargoExecutable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ManifestPath)) {
    $ManifestPath = Join-Path $repoRoot "src-tauri\Cargo.toml"
}
$ManifestPath = [System.IO.Path]::GetFullPath($ManifestPath)
if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
    throw "Cargo feature boundary manifest does not exist: $ManifestPath"
}

if ([string]::IsNullOrWhiteSpace($CargoExecutable)) {
    $cargoCommand = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if (-not $cargoCommand) {
        $cargoCommand = Get-Command cargo -ErrorAction Stop
    }
    $CargoExecutable = $cargoCommand.Source
}
if (-not (Test-Path -LiteralPath $CargoExecutable -PathType Leaf)) {
    $resolvedCargo = Get-Command $CargoExecutable -ErrorAction SilentlyContinue
    if (-not $resolvedCargo) {
        throw "Cargo executable was not found: $CargoExecutable"
    }
    $CargoExecutable = $resolvedCargo.Source
}

function Get-CargoFeatureGraph {
    param(
        [Parameter(Mandatory)] [string]$Package,
        [string[]]$Features = @(),
        [string]$InvertPackage
    )

    $arguments = @(
        "tree",
        "--locked",
        "--manifest-path", $ManifestPath,
        "--package", $Package,
        "--edges", "features",
        "--no-default-features",
        "--prefix", "none",
        "--charset", "ascii"
    )
    if ($Features.Count -gt 0) {
        $arguments += @("--features", ($Features -join ","))
    }
    if (-not [string]::IsNullOrWhiteSpace($InvertPackage)) {
        $arguments += @("--invert", $InvertPackage)
    }

    $output = @(& $CargoExecutable @arguments 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo tree failed for package '$Package' with exit code $LASTEXITCODE.`n$($output -join [Environment]::NewLine)"
    }

    $names = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    $featureEdges = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($line in $output) {
        if ([string]$line -match '^(?<name>[A-Za-z0-9_-]+)\s+v\d') {
            [void]$names.Add([string]$Matches.name)
        }
        if ([string]$line -match '^(?<name>[A-Za-z0-9_-]+)\s+feature\s+"(?<feature>[^"]+)"') {
            [void]$featureEdges.Add("$($Matches.name):$($Matches.feature)")
        }
    }
    if ($names.Count -eq 0) {
        throw "cargo tree returned no parseable packages for '$Package'."
    }
    return [pscustomobject]@{
        packages = @($names | Sort-Object)
        featureEdges = @($featureEdges | Sort-Object)
    }
}

$mainForbiddenPackages = @("tiff", "libheif-rs", "libheif-sys")
$mainForbiddenFeatureEdges = @("image:tiff")
$mainRequiredBasePackages = @("imgviewer-codec-core", "image")
$mainRequiredBaseFeatureEdges = @("image:gif", "image:jpeg", "image:png", "image:webp")
$helperRequiredPackages = @("image", "tiff", "libheif-rs", "libheif-sys")
$helperRequiredFeatureEdges = @("image:tiff")
$helperFeatures = @("heic", "tiff")

$mainGraph = Get-CargoFeatureGraph -Package "imgviewer"
$unexpectedMainPackages = @(
    $mainForbiddenPackages | Where-Object { $mainGraph.packages -ccontains $_ }
)
if ($unexpectedMainPackages.Count -gt 0) {
    throw "The ImgViewer process Cargo graph reaches isolated codec packages: $($unexpectedMainPackages -join ', ')"
}
$unexpectedMainFeatures = @(
    $mainForbiddenFeatureEdges | Where-Object { $mainGraph.featureEdges -ccontains $_ }
)
if ($unexpectedMainFeatures.Count -gt 0) {
    throw "The ImgViewer process Cargo graph enables isolated codec features: $($unexpectedMainFeatures -join ', ')"
}
$missingMainBasePackages = @(
    $mainRequiredBasePackages | Where-Object { $mainGraph.packages -cnotcontains $_ }
)
$missingMainBaseFeatures = @(
    $mainRequiredBaseFeatureEdges | Where-Object { $mainGraph.featureEdges -cnotcontains $_ }
)
if ($missingMainBasePackages.Count -gt 0 -or $missingMainBaseFeatures.Count -gt 0) {
    throw "The ImgViewer process Cargo graph is missing the approved base image graph: packages=$($missingMainBasePackages -join ',') features=$($missingMainBaseFeatures -join ',')"
}

$helperGraph = Get-CargoFeatureGraph `
    -Package "imgviewer-codec-helper" `
    -Features $helperFeatures
$helperTiffGraph = Get-CargoFeatureGraph `
    -Package "imgviewer-codec-helper" `
    -Features $helperFeatures `
    -InvertPackage "tiff"
$missingHelperPackages = @(
    $helperRequiredPackages | Where-Object { $helperGraph.packages -cnotcontains $_ }
)
if ($missingHelperPackages.Count -gt 0) {
    throw "The codec helper Cargo graph is missing required HEIF/TIFF packages: $($missingHelperPackages -join ', ')"
}
$missingHelperFeatures = @(
    $helperRequiredFeatureEdges | Where-Object { $helperTiffGraph.featureEdges -cnotcontains $_ }
)
if ($missingHelperFeatures.Count -gt 0) {
    throw "The codec helper Cargo graph is missing required TIFF feature edges: $($missingHelperFeatures -join ', ')"
}

$result = [ordered]@{
    main = [ordered]@{
        package = "imgviewer"
        baseImagePresent = [bool]($mainGraph.packages -ccontains "image")
        requiredBaseFeatureEdgesPresent = @($mainRequiredBaseFeatureEdges)
        isolatedPackagesPresent = @($unexpectedMainPackages)
        isolatedFeatureEdgesPresent = @($unexpectedMainFeatures)
    }
    helper = [ordered]@{
        package = "imgviewer-codec-helper"
        cargoFeatures = $helperFeatures
        requiredPackagesPresent = @($helperRequiredPackages)
        requiredFeatureEdgesPresent = @($helperRequiredFeatureEdges)
    }
}
Write-Host "PASS codec-feature-boundary main-heif=absent main-tiff=absent helper-heif=present helper-tiff=present"
$result | ConvertTo-Json -Depth 5
