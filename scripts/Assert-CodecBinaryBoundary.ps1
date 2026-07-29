#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$StageDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$forbiddenCodecFilePattern =
    '^(?i:(?:lib)?(?:x265|aom|avif|dav1d|rav1e|svt[-_]?av1|kvazaar|vvenc))[^\\/]*\.(?:dll|exe)$'
$protectedHeifImportPattern = '^(?i:(?:lib)?heif|libde265)\.dll$'

function Get-DumpbinPath {
    if ($env:DUMPBIN_EXE -and (Test-Path -LiteralPath $env:DUMPBIN_EXE -PathType Leaf)) {
        return [System.IO.Path]::GetFullPath($env:DUMPBIN_EXE)
    }

    $command = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $vswhereCandidates = @()
    if ($env:VSWHERE_EXE) {
        $vswhereCandidates += $env:VSWHERE_EXE
    }
    if (${env:ProgramFiles(x86)}) {
        $vswhereCandidates += (
            Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
        )
    }

    foreach ($vswhere in $vswhereCandidates | Select-Object -Unique) {
        if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
            continue
        }
        $matches = @(
            & $vswhere -latest -products * `
                -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe"
        )
        $match = $matches |
            Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } |
            Select-Object -First 1
        if ($match) {
            return [System.IO.Path]::GetFullPath($match)
        }
    }

    throw "dumpbin.exe was not found. Install the MSVC x64 tools, or set DUMPBIN_EXE."
}

function Get-ImportedDlls {
    param(
        [Parameter(Mandatory)] [string]$BinaryPath,
        [Parameter(Mandatory)] [string]$DumpbinPath
    )

    $output = @(& $DumpbinPath /nologo /dependents $BinaryPath 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "dumpbin failed for $BinaryPath with exit code $LASTEXITCODE.`n$($output -join [Environment]::NewLine)"
    }

    return @(
        $output |
            ForEach-Object {
                if ($_ -match '^\s+([A-Za-z0-9_.+-]+\.dll)\s*$') {
                    $Matches[1]
                }
            } |
            Where-Object { $_ } |
            Sort-Object -Unique
    )
}

function Get-StagedDllIndex {
    param([Parameter(Mandatory)] [string]$Directory)

    $index = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($file in Get-ChildItem -LiteralPath $Directory -Recurse -File -Filter "*.dll") {
        if ($index.ContainsKey($file.Name)) {
            throw "Codec import graph is ambiguous; duplicate staged DLL name: $($file.Name)"
        }
        $index.Add($file.Name, $file.FullName)
    }
    return $index
}

function Get-ImportGraph {
    param(
        [Parameter(Mandatory)] [string]$RootBinary,
        [Parameter(Mandatory)] [string]$DumpbinPath,
        [Parameter(Mandatory)]
        [System.Collections.Generic.Dictionary[string, string]]$StagedDlls
    )

    $rootName = [System.IO.Path]::GetFileName($RootBinary)
    $queue = [System.Collections.Queue]::new()
    $queue.Enqueue([pscustomobject]@{
        path = $RootBinary
        chain = @($rootName)
    })
    $visited = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $reachable = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $edges = [System.Collections.ArrayList]::new()
    $protectedPaths = [System.Collections.ArrayList]::new()
    $rootImports = @()

    while ($queue.Count -gt 0) {
        $item = $queue.Dequeue()
        $binaryPath = [string]$item.path
        if (-not $visited.Add($binaryPath)) {
            continue
        }

        $binaryName = [System.IO.Path]::GetFileName($binaryPath)
        $imports = @(Get-ImportedDlls -BinaryPath $binaryPath -DumpbinPath $DumpbinPath)
        if ($binaryPath -ieq $RootBinary) {
            $rootImports = $imports
        }
        foreach ($import in $imports) {
            [void]$reachable.Add($import)
            $resolvedPath = $null
            if ($StagedDlls.ContainsKey($import)) {
                $resolvedPath = $StagedDlls[$import]
            }
            [void]$edges.Add([ordered]@{
                from = $binaryName
                to = $import
                staged = [bool]$resolvedPath
            })

            $nextChain = @($item.chain) + @($import)
            if ($import -match $protectedHeifImportPattern) {
                [void]$protectedPaths.Add(($nextChain -join " -> "))
            }
            if ($resolvedPath) {
                $queue.Enqueue([pscustomobject]@{
                    path = $resolvedPath
                    chain = $nextChain
                })
            }
        }
    }

    return [pscustomobject]@{
        rootImports = @($rootImports)
        reachableImports = @($reachable | Sort-Object)
        edges = @($edges)
        protectedCodecPaths = @($protectedPaths | Sort-Object -Unique)
    }
}

$StageDirectory = [System.IO.Path]::GetFullPath($StageDirectory)
if (-not (Test-Path -LiteralPath $StageDirectory -PathType Container)) {
    throw "Stage directory does not exist: $StageDirectory"
}

$mainPath = Join-Path $StageDirectory "ImgViewer.exe"
$helperPath = Join-Path $StageDirectory "ImgViewer.CodecHelper.exe"
foreach ($required in @($mainPath, $helperPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Codec binary boundary cannot be checked; executable is missing: $required"
    }
}

$forbiddenCodecFiles = @(
    Get-ChildItem -LiteralPath $StageDirectory -Recurse -File |
        Where-Object { $_.Name -match $forbiddenCodecFilePattern }
)
if ($forbiddenCodecFiles.Count -gt 0) {
    throw "Portable package contains an unapproved HEIF/AVIF codec: $($forbiddenCodecFiles.Name -join ', ')"
}

$dumpbin = Get-DumpbinPath
$stagedDlls = Get-StagedDllIndex -Directory $StageDirectory
$mainGraph = Get-ImportGraph -RootBinary $mainPath -DumpbinPath $dumpbin `
    -StagedDlls $stagedDlls
$helperGraph = Get-ImportGraph -RootBinary $helperPath -DumpbinPath $dumpbin `
    -StagedDlls $stagedDlls

if ($mainGraph.protectedCodecPaths.Count -gt 0) {
    throw "ImgViewer.exe must not reach native HEIF codecs through its import graph: $($mainGraph.protectedCodecPaths -join '; ')"
}
if ($helperGraph.reachableImports -inotcontains "heif.dll") {
    throw "ImgViewer.CodecHelper.exe import graph must reach heif.dll."
}

$result = [ordered]@{
    main = [ordered]@{
        fileName = "ImgViewer.exe"
        forbiddenCodecPaths = @($mainGraph.protectedCodecPaths)
        imports = @($mainGraph.rootImports)
        reachableImports = @($mainGraph.reachableImports)
        edges = @($mainGraph.edges)
    }
    helper = [ordered]@{
        fileName = "ImgViewer.CodecHelper.exe"
        requiredCodecImport = "heif.dll"
        imports = @($helperGraph.rootImports)
        reachableImports = @($helperGraph.reachableImports)
        edges = @($helperGraph.edges)
    }
}
$result | ConvertTo-Json -Depth 6
