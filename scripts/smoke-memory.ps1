[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [string]$FixtureDirectory,

    [switch]$IsolatedCodecAlternation,

    [ValidateRange(3, 10000)]
    [int]$Cycles = 100,

    [ValidateRange(1, 32768)]
    [int]$Width = 2048,

    [ValidateRange(1, 32768)]
    [int]$Height = 1536,

    [ValidateRange(0, 9999)]
    [int]$Warmup = 70,

    [ValidateRange(0, 60000)]
    [int]$IdleMilliseconds = 100,

    [ValidateRange(1, 300)]
    [int]$TimeoutSeconds = 20,

    [string]$OutputCsv,

    [ValidateRange(0, 8192)]
    [double]$MaxRetainedPrivateMiB = 128,

    [ValidateRange(0, 8192)]
    [double]$MaxRetainedWorkingSetMiB = 160,

    [ValidateRange(0, 1024)]
    [double]$MaxPrivateSlopeMiBPerCycle = 2,

    [ValidateRange(0, 1024)]
    [double]$MaxWorkingSetSlopeMiBPerCycle = 3,

    [ValidateRange(1, 60000)]
    [int]$MaxP95LoadMilliseconds = 10000
)

# LoadMilliseconds measures second-instance launch until both the handoff
# process exits and UI Automation exposes the requested image. The fixed idle
# occurs afterwards and is deliberately excluded from the p95 load metric.
# The long default warm-up lets WebView2 reach its normal image-cache/GC
# pressure plateau; leak thresholds are evaluated only on the remaining tail.
# IsolatedCodecAlternation replaces the generated PNG set with committed TIFF
# and HEIF fixtures so every measured cycle exercises the shared codec helper.
# When OutputCsv is omitted, the CSV remains in the system TEMP directory;
# only generated image fixtures are removed in finally.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($FixtureDirectory)) {
    $FixtureDirectory = Join-Path $PSScriptRoot '..\tests\fixtures'
}

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class ImgViewerMemorySmokeNative
{
    public sealed class ProcessSnapshotEntry
    {
        public uint ProcessId { get; set; }
        public uint ParentProcessId { get; set; }
        public string Name { get; set; }
        public ulong WorkingSetSize { get; set; }
        public ulong PrivatePageCount { get; set; }
        public bool CountersAvailable { get; set; }
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct ProcessEntry32
    {
        public uint Size;
        public uint Usage;
        public uint ProcessId;
        public IntPtr DefaultHeapId;
        public uint ModuleId;
        public uint Threads;
        public uint ParentProcessId;
        public int BasePriority;
        public uint Flags;

        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string ExeFile;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ProcessMemoryCountersEx
    {
        public uint Size;
        public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize;
        public UIntPtr WorkingSetSize;
        public UIntPtr QuotaPeakPagedPoolUsage;
        public UIntPtr QuotaPagedPoolUsage;
        public UIntPtr QuotaPeakNonPagedPoolUsage;
        public UIntPtr QuotaNonPagedPoolUsage;
        public UIntPtr PagefileUsage;
        public UIntPtr PeakPagefileUsage;
        public UIntPtr PrivateUsage;
    }

    public delegate bool EnumWindowsCallback(IntPtr hwnd, IntPtr state);

    private static readonly IntPtr InvalidHandleValue = new IntPtr(-1);
    private const uint SnapshotProcesses = 0x00000002;
    private const uint QueryLimitedInformation = 0x00001000;
    private const uint VirtualMemoryRead = 0x00000010;

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr state);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int capacity);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool Process32FirstW(IntPtr snapshot, ref ProcessEntry32 entry);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool Process32NextW(IntPtr snapshot, ref ProcessEntry32 entry);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint desiredAccess, bool inheritHandle, uint processId);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("psapi.dll", SetLastError = true)]
    private static extern bool GetProcessMemoryInfo(
        IntPtr process,
        out ProcessMemoryCountersEx counters,
        uint size
    );

    public static IntPtr FindMainWindow(int expectedProcessId)
    {
        IntPtr result = IntPtr.Zero;
        EnumWindows(delegate (IntPtr hwnd, IntPtr state)
        {
            uint processId;
            GetWindowThreadProcessId(hwnd, out processId);
            if (processId != (uint)expectedProcessId || !IsWindowVisible(hwnd))
                return true;

            var title = new StringBuilder(256);
            GetWindowTextW(hwnd, title, title.Capacity);
            if (String.Equals(title.ToString(), "ImgViewer", StringComparison.Ordinal))
            {
                result = hwnd;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return result;
    }

    public static ProcessSnapshotEntry[] ReadProcessSnapshot()
    {
        IntPtr snapshot = CreateToolhelp32Snapshot(SnapshotProcesses, 0);
        if (snapshot == InvalidHandleValue)
            throw new InvalidOperationException(
                "CreateToolhelp32Snapshot failed with Win32 error " + Marshal.GetLastWin32Error() + "."
            );

        var result = new List<ProcessSnapshotEntry>();
        try
        {
            var nativeEntry = new ProcessEntry32();
            nativeEntry.Size = (uint)Marshal.SizeOf(typeof(ProcessEntry32));
            if (!Process32FirstW(snapshot, ref nativeEntry))
                throw new InvalidOperationException(
                    "Process32FirstW failed with Win32 error " + Marshal.GetLastWin32Error() + "."
                );

            do
            {
                var entry = new ProcessSnapshotEntry
                {
                    ProcessId = nativeEntry.ProcessId,
                    ParentProcessId = nativeEntry.ParentProcessId,
                    Name = nativeEntry.ExeFile ?? String.Empty
                };
                IntPtr process = OpenProcess(
                    QueryLimitedInformation | VirtualMemoryRead,
                    false,
                    nativeEntry.ProcessId
                );
                if (process != IntPtr.Zero)
                {
                    try
                    {
                        var counters = new ProcessMemoryCountersEx();
                        counters.Size = (uint)Marshal.SizeOf(typeof(ProcessMemoryCountersEx));
                        if (GetProcessMemoryInfo(process, out counters, counters.Size))
                        {
                            entry.WorkingSetSize = counters.WorkingSetSize.ToUInt64();
                            // PrivateUsage is the byte form of the private-page
                            // counter exposed by Win32_Process.PrivatePageCount.
                            entry.PrivatePageCount = counters.PrivateUsage.ToUInt64();
                            entry.CountersAvailable = true;
                        }
                    }
                    finally
                    {
                        CloseHandle(process);
                    }
                }
                result.Add(entry);
                nativeEntry.Size = (uint)Marshal.SizeOf(typeof(ProcessEntry32));
            }
            while (Process32NextW(snapshot, ref nativeEntry));
        }
        finally
        {
            CloseHandle(snapshot);
        }
        return result.ToArray();
    }
}
'@

function Resolve-ExistingFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (-not [IO.File]::Exists($resolved.Path)) {
        throw "File does not exist: $Path"
    }
    return [IO.Path]::GetFullPath($resolved.Path)
}

function Resolve-OutputPath {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        $stamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
        return [IO.Path]::Combine(
            [IO.Path]::GetTempPath(),
            "ImgViewer-memory-smoke-$stamp-$PID.csv"
        )
    }

    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path ([Environment]::CurrentDirectory) $Path))
}

function Test-NamedProcessRunning {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string]$ExpectedProcessName
    )

    $candidate = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    return (
        $null -ne $candidate -and
        [string]::Equals(
            [string]$candidate.ProcessName,
            $ExpectedProcessName,
            [StringComparison]::OrdinalIgnoreCase
        )
    )
}

function Wait-Until {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Condition,
        [Parameter(Mandatory = $true)][string]$FailureMessage
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $value = & $Condition
        if ($null -ne $value -and $value -ne $false) {
            return $value
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $FailureMessage
}

function Find-ViewerImage {
    param(
        [Parameter(Mandatory = $true)][IntPtr]$Window,
        [Parameter(Mandatory = $true)][string]$Name
    )

    try {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
        if ($null -eq $root) {
            return $null
        }
        $nameCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            $Name
        )
        $typeCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Image
        )
        $condition = [System.Windows.Automation.AndCondition]::new($nameCondition, $typeCondition)
        return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    catch {
        # The WebView2 child HWND can be replaced during startup. A later poll
        # is authoritative; an unavailable element is not a rendered image.
        return $null
    }
}

function Wait-ViewerImage {
    param(
        [Parameter(Mandatory = $true)][IntPtr]$Window,
        [Parameter(Mandatory = $true)][string]$Name
    )

    return Wait-Until -FailureMessage "Timed out waiting for rendered image '$Name'." -Condition {
        Find-ViewerImage -Window $Window -Name $Name
    }
}

function New-DeterministicImages {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][int]$ImageWidth,
        [Parameter(Mandatory = $true)][int]$ImageHeight
    )

    $definitions = @(
        @{
            Name = 'memory-red.png'
            Background = [System.Drawing.Color]::FromArgb(255, 176, 44, 44)
            Accent = [System.Drawing.Color]::FromArgb(255, 255, 214, 102)
        },
        @{
            Name = 'memory-green.png'
            Background = [System.Drawing.Color]::FromArgb(255, 35, 132, 86)
            Accent = [System.Drawing.Color]::FromArgb(255, 126, 232, 190)
        },
        @{
            Name = 'memory-blue.png'
            Background = [System.Drawing.Color]::FromArgb(255, 41, 91, 180)
            Accent = [System.Drawing.Color]::FromArgb(255, 155, 201, 255)
        }
    )

    $paths = [Collections.Generic.List[string]]::new()
    foreach ($definition in $definitions) {
        $path = Join-Path $Directory $definition.Name
        $bitmap = [System.Drawing.Bitmap]::new(
            $ImageWidth,
            $ImageHeight,
            [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
        )
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.Clear($definition.Background)
            $accentBrush = [System.Drawing.SolidBrush]::new($definition.Accent)
            try {
                # A deterministic stripe pattern prevents every fixture from
                # collapsing to the exact same trivial decoder workload.
                for ($stripe = 0; $stripe -lt 16; $stripe += 2) {
                    $left = [int][Math]::Floor($stripe * $ImageWidth / 16.0)
                    $right = [int][Math]::Floor(($stripe + 1) * $ImageWidth / 16.0)
                    $graphics.FillRectangle($accentBrush, $left, 0, ($right - $left), $ImageHeight)
                }
            }
            finally {
                $accentBrush.Dispose()
            }
            $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
        }
        finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
        $paths.Add($path)
    }
    return $paths.ToArray()
}

function Get-ImgViewerProcessSample {
    param(
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [ValidateRange(1, 3)][int]$Attempt = 1
    )

    $snapshot = @([ImgViewerMemorySmokeNative]::ReadProcessSnapshot())
    $root = $snapshot | Where-Object { [int]$_.ProcessId -eq $RootProcessId } | Select-Object -First 1
    if ($null -eq $root) {
        throw "ImgViewer process $RootProcessId is no longer running."
    }
    if (-not $root.CountersAvailable) {
        if ($Attempt -lt 3) {
            Start-Sleep -Milliseconds 25
            return Get-ImgViewerProcessSample -RootProcessId $RootProcessId -Attempt ($Attempt + 1)
        }
        throw "Unable to read memory counters for ImgViewer process $RootProcessId."
    }

    $descendantIds = [Collections.Generic.HashSet[uint32]]::new()
    [void]$descendantIds.Add([uint32]$RootProcessId)
    do {
        $added = $false
        foreach ($item in $snapshot) {
            $processId = [uint32]$item.ProcessId
            $parentId = [uint32]$item.ParentProcessId
            if ($processId -ne 0 -and $descendantIds.Contains($parentId) -and
                -not $descendantIds.Contains($processId)) {
                [void]$descendantIds.Add($processId)
                $added = $true
            }
        }
    } while ($added)

    $codecHelpers = @(
        $snapshot | Where-Object {
            $processId = [uint32]$_.ProcessId
            $descendantIds.Contains($processId) -and
                [string]::Equals(
                    [string]$_.Name,
                    'ImgViewer.CodecHelper.exe',
                    [StringComparison]::OrdinalIgnoreCase
                )
        }
    )
    $nonDirectHelpers = @(
        $codecHelpers |
            Where-Object { [uint32]$_.ParentProcessId -ne [uint32]$RootProcessId }
    )
    if ($nonDirectHelpers.Count -gt 0) {
        $ids = @($nonDirectHelpers | ForEach-Object { $_.ProcessId })
        throw "Codec helper must be a direct ImgViewer child; invalid PIDs: $($ids -join ', ')."
    }
    if ($codecHelpers.Count -gt 1) {
        $ids = @($codecHelpers | ForEach-Object { $_.ProcessId })
        throw "ImgViewer PID $RootProcessId has multiple codec helper children: $($ids -join ', ')."
    }

    $selectedCandidates = @(
        $snapshot | Where-Object {
            $processId = [uint32]$_.ProcessId
            $processId -eq [uint32]$RootProcessId -or
                ($descendantIds.Contains($processId) -and
                    (
                        [string]::Equals(
                            $_.Name,
                            'msedgewebview2.exe',
                            [StringComparison]::OrdinalIgnoreCase
                        ) -or
                        [string]::Equals(
                            $_.Name,
                            'ImgViewer.CodecHelper.exe',
                            [StringComparison]::OrdinalIgnoreCase
                        )
                    ))
        }
    )
    $unreadable = @($selectedCandidates | Where-Object { -not $_.CountersAvailable })
    if ($unreadable.Count -gt 0) {
        if ($Attempt -lt 3) {
            Start-Sleep -Milliseconds 25
            return Get-ImgViewerProcessSample -RootProcessId $RootProcessId -Attempt ($Attempt + 1)
        }
        $unreadableNames = @($unreadable | ForEach-Object { "$($_.Name):$($_.ProcessId)" })
        throw "Unable to read every ImgViewer/WebView2/helper memory counter: $($unreadableNames -join ', ')."
    }
    $selected = @($selectedCandidates | Where-Object { $_.CountersAvailable })
    $webViews = @(
        $selected | Where-Object {
            [string]::Equals(
                $_.Name,
                'msedgewebview2.exe',
                [StringComparison]::OrdinalIgnoreCase
            )
        }
    )
    $helpers = @(
        $selected | Where-Object {
            [string]::Equals(
                $_.Name,
                'ImgViewer.CodecHelper.exe',
                [StringComparison]::OrdinalIgnoreCase
            )
        }
    )

    [uint64]$workingSetBytes = 0
    [uint64]$privateBytes = 0
    foreach ($item in $selected) {
        $workingSetBytes += [uint64]$item.WorkingSetSize
        $privateBytes += [uint64]$item.PrivatePageCount
    }
    [uint64]$webViewWorkingSetBytes = 0
    [uint64]$webViewPrivateBytes = 0
    foreach ($item in $webViews) {
        $webViewWorkingSetBytes += [uint64]$item.WorkingSetSize
        $webViewPrivateBytes += [uint64]$item.PrivatePageCount
    }
    [uint64]$helperWorkingSetBytes = 0
    [uint64]$helperPrivateBytes = 0
    foreach ($item in $helpers) {
        $helperWorkingSetBytes += [uint64]$item.WorkingSetSize
        $helperPrivateBytes += [uint64]$item.PrivatePageCount
    }

    return [pscustomobject]@{
        WorkingSetBytes = $workingSetBytes
        PrivateBytes = $privateBytes
        RootWorkingSetBytes = [uint64]$root.WorkingSetSize
        RootPrivateBytes = [uint64]$root.PrivatePageCount
        WebViewWorkingSetBytes = $webViewWorkingSetBytes
        WebViewPrivateBytes = $webViewPrivateBytes
        HelperWorkingSetBytes = $helperWorkingSetBytes
        HelperPrivateBytes = $helperPrivateBytes
        ProcessCount = $selected.Count
        WebViewProcessCount = $webViews.Count
        HelperProcessCount = $helpers.Count
        DescendantProcessIds = [uint32[]]$descendantIds
        MeasuredProcessIds = [uint32[]]@($selected | ForEach-Object { [uint32]$_.ProcessId })
        HelperProcessIds = [uint32[]]@($helpers | ForEach-Object { [uint32]$_.ProcessId })
    }
}

function Open-ThroughExistingInstance {
    param(
        [Parameter(Mandatory = $true)][string]$ExecutablePath,
        [Parameter(Mandatory = $true)][string]$ImagePath,
        [Parameter(Mandatory = $true)][IntPtr]$Window,
        [Parameter(Mandatory = $true)][int]$RootProcessId
    )

    $handoff = $null
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    [uint64]$peakWorkingSetBytes = 0
    [uint64]$peakPrivateBytes = 0
    [uint64]$peakRootWorkingSetBytes = 0
    [uint64]$peakRootPrivateBytes = 0
    [uint64]$peakHelperWorkingSetBytes = 0
    [uint64]$peakHelperPrivateBytes = 0
    try {
        $handoff = Start-Process -FilePath $ExecutablePath `
            -ArgumentList ('"' + $ImagePath + '"') `
            -PassThru
        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $imageReady = $false
        $handoffExited = $false
        do {
            $handoff.Refresh()
            $handoffExited = $handoff.HasExited
            $memory = Get-ImgViewerProcessSample -RootProcessId $RootProcessId
            $peakWorkingSetBytes = [Math]::Max($peakWorkingSetBytes, $memory.WorkingSetBytes)
            $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $memory.PrivateBytes)
            $peakRootWorkingSetBytes = [Math]::Max($peakRootWorkingSetBytes, $memory.RootWorkingSetBytes)
            $peakRootPrivateBytes = [Math]::Max($peakRootPrivateBytes, $memory.RootPrivateBytes)
            $peakHelperWorkingSetBytes = [Math]::Max(
                $peakHelperWorkingSetBytes,
                $memory.HelperWorkingSetBytes
            )
            $peakHelperPrivateBytes = [Math]::Max(
                $peakHelperPrivateBytes,
                $memory.HelperPrivateBytes
            )
            if (-not $imageReady) {
                $imageReady = $null -ne (Find-ViewerImage -Window $Window -Name ([IO.Path]::GetFileName($ImagePath)))
            }
            if ($imageReady -and $handoffExited) {
                break
            }
            Start-Sleep -Milliseconds 25
        } while ([DateTime]::UtcNow -lt $deadline)
        $stopwatch.Stop()

        if (-not $imageReady) {
            throw "Timed out waiting for rendered image '$([IO.Path]::GetFileName($ImagePath))'."
        }
        if (-not $handoffExited) {
            throw "Second ImgViewer instance did not exit after handing off '$ImagePath'."
        }
        if ($handoff.ExitCode -ne 0) {
            throw "Second ImgViewer instance exited with code $($handoff.ExitCode)."
        }
        $memory = Get-ImgViewerProcessSample -RootProcessId $RootProcessId
        $peakWorkingSetBytes = [Math]::Max($peakWorkingSetBytes, $memory.WorkingSetBytes)
        $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $memory.PrivateBytes)
        $peakRootWorkingSetBytes = [Math]::Max($peakRootWorkingSetBytes, $memory.RootWorkingSetBytes)
        $peakRootPrivateBytes = [Math]::Max($peakRootPrivateBytes, $memory.RootPrivateBytes)
        $peakHelperWorkingSetBytes = [Math]::Max(
            $peakHelperWorkingSetBytes,
            $memory.HelperWorkingSetBytes
        )
        $peakHelperPrivateBytes = [Math]::Max(
            $peakHelperPrivateBytes,
            $memory.HelperPrivateBytes
        )
        return [pscustomobject]@{
            LoadMilliseconds = [int][Math]::Ceiling($stopwatch.Elapsed.TotalMilliseconds)
            PeakWorkingSetBytes = $peakWorkingSetBytes
            PeakPrivateBytes = $peakPrivateBytes
            PeakRootWorkingSetBytes = $peakRootWorkingSetBytes
            PeakRootPrivateBytes = $peakRootPrivateBytes
            PeakHelperWorkingSetBytes = $peakHelperWorkingSetBytes
            PeakHelperPrivateBytes = $peakHelperPrivateBytes
        }
    }
    finally {
        $stopwatch.Stop()
        if ($null -ne $handoff) {
            try {
                $handoff.Refresh()
                if (-not $handoff.HasExited) {
                    $handoff.Kill()
                    [void]$handoff.WaitForExit(2000)
                }
            }
            catch {}
            $handoff.Dispose()
        }
    }
}

function Get-Percentile {
    param(
        [Parameter(Mandatory = $true)][double[]]$Values,
        [Parameter(Mandatory = $true)][double]$Percentile
    )

    if ($Values.Count -eq 0) {
        throw 'Cannot calculate a percentile for an empty sample.'
    }
    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($Percentile * $sorted.Count) - 1)
    return [double]$sorted[$index]
}

function Get-Median {
    param([Parameter(Mandatory = $true)][double[]]$Values)

    $sorted = @($Values | Sort-Object)
    if ($sorted.Count -eq 0) {
        throw 'Cannot calculate a median for an empty sample.'
    }
    $middle = [int][Math]::Floor($sorted.Count / 2)
    if (($sorted.Count % 2) -eq 1) {
        return [double]$sorted[$middle]
    }
    return ([double]$sorted[$middle - 1] + [double]$sorted[$middle]) / 2.0
}

function Get-RetainedGrowthMiB {
    param(
        [Parameter(Mandatory = $true)][object[]]$Samples,
        [Parameter(Mandatory = $true)][string]$Property
    )

    $windowSize = [Math]::Min(3, [Math]::Max(1, [Math]::Floor($Samples.Count / 3)))
    [double[]]$head = @(
        $Samples | Select-Object -First $windowSize | ForEach-Object { [double]$_.$Property / 1MB }
    )
    [double[]]$tail = @(
        $Samples | Select-Object -Last $windowSize | ForEach-Object { [double]$_.$Property / 1MB }
    )
    return (Get-Median -Values $tail) - (Get-Median -Values $head)
}

function Get-SlopeMiBPerCycle {
    param(
        [Parameter(Mandatory = $true)][object[]]$Samples,
        [Parameter(Mandatory = $true)][string]$Property
    )

    if ($Samples.Count -lt 2) {
        return 0.0
    }
    $meanX = ($Samples | Measure-Object -Property Cycle -Average).Average
    $valuesMiB = @($Samples | ForEach-Object { [double]$_.$Property / 1MB })
    $meanY = ($valuesMiB | Measure-Object -Average).Average
    [double]$numerator = 0
    [double]$denominator = 0
    for ($index = 0; $index -lt $Samples.Count; $index++) {
        $deltaX = [double]$Samples[$index].Cycle - $meanX
        $deltaY = [double]$valuesMiB[$index] - $meanY
        $numerator += $deltaX * $deltaY
        $denominator += $deltaX * $deltaX
    }
    if ($denominator -eq 0) {
        return 0.0
    }
    return $numerator / $denominator
}

if ($Warmup -ge $Cycles) {
    throw "Warmup ($Warmup) must be smaller than Cycles ($Cycles)."
}
if (([int64]$Width * [int64]$Height) -gt 100000000) {
    throw 'Width multiplied by Height must not exceed the viewer 100,000,000-pixel limit.'
}

$executablePath = Resolve-ExistingFile -Path $Executable
if (-not [string]::Equals([IO.Path]::GetExtension($executablePath), '.exe', [StringComparison]::OrdinalIgnoreCase)) {
    throw "Executable must point to an .exe file: $executablePath"
}
$helperExecutablePath = Resolve-ExistingFile -Path (
    Join-Path ([IO.Path]::GetDirectoryName($executablePath)) 'ImgViewer.CodecHelper.exe'
)
$fixtureDirectoryPath = [IO.Path]::GetFullPath(
    (Resolve-Path -LiteralPath $FixtureDirectory -ErrorAction Stop).Path
)
if (-not [IO.Directory]::Exists($fixtureDirectoryPath)) {
    throw "Fixture directory does not exist: $FixtureDirectory"
}
$helperFixtureSource = Resolve-ExistingFile -Path (
    Join-Path $fixtureDirectoryPath 'primary-second.heic'
)
$viewerProcessName = [IO.Path]::GetFileNameWithoutExtension($executablePath)
$helperProcessName = [IO.Path]::GetFileNameWithoutExtension($helperExecutablePath)
$existingViewers = @(Get-Process -Name $viewerProcessName -ErrorAction SilentlyContinue)
if ($existingViewers.Count -gt 0) {
    $existingIds = @($existingViewers | ForEach-Object { $_.Id })
    throw "Close existing $viewerProcessName instances before running the memory smoke (PIDs: $($existingIds -join ', '))."
}
$existingHelpers = @(Get-Process -Name $helperProcessName -ErrorAction SilentlyContinue)
if ($existingHelpers.Count -gt 0) {
    $existingIds = @($existingHelpers | ForEach-Object { $_.Id })
    throw "Close orphaned $helperProcessName instances before running the memory smoke (PIDs: $($existingIds -join ', '))."
}
$csvPath = Resolve-OutputPath -Path $OutputCsv
$csvDirectory = [IO.Path]::GetDirectoryName($csvPath)
if ([string]::IsNullOrWhiteSpace($csvDirectory)) {
    throw "Unable to resolve the CSV output directory for '$csvPath'."
}
[IO.Directory]::CreateDirectory($csvDirectory) | Out-Null

$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$runDirectory = Join-Path $tempRoot ("ImgViewer-memory-smoke-" + [Guid]::NewGuid().ToString('N'))
$mainProcess = $null
$window = [IntPtr]::Zero
$lastTreeProcessIds = @()
$helperProcessIds = @()
$expectedHelperProcessId = $null
$helperCleanupFailure = $null
$memorySmokeCompleted = $false
$rows = [Collections.Generic.List[object]]::new()

try {
    [IO.Directory]::CreateDirectory($runDirectory) | Out-Null
    if ($IsolatedCodecAlternation) {
        $tiffFixtureSource = Resolve-ExistingFile -Path (
            Join-Path $fixtureDirectoryPath 'two-page.tiff'
        )
        $tiffFixturePath = Join-Path $runDirectory 'memory-codec-a.tiff'
        $heifFixturePath = Join-Path $runDirectory 'memory-codec-b.heic'
        [IO.File]::Copy($tiffFixtureSource, $tiffFixturePath, $false)
        [IO.File]::Copy($helperFixtureSource, $heifFixturePath, $false)
        $images = @($tiffFixturePath, $heifFixturePath)
        $helperFixturePath = $tiffFixturePath
        $helperSourceLabel = 'tiff-heif'
        $summaryWidth = '5x3+3'
        $summaryHeight = '5'
    }
    else {
        $images = @(New-DeterministicImages -Directory $runDirectory -ImageWidth $Width -ImageHeight $Height)
        if ($images.Count -lt 3) {
            throw "Expected at least three generated images; got $($images.Count)."
        }
        $helperFixturePath = Join-Path $runDirectory 'memory-helper-primary.heic'
        [IO.File]::Copy($helperFixtureSource, $helperFixturePath, $false)
        $helperSourceLabel = 'heic'
        $summaryWidth = $Width
        $summaryHeight = $Height
    }

    $mainProcess = Start-Process -FilePath $executablePath `
        -ArgumentList ('"' + $helperFixturePath + '"') `
        -PassThru
    $window = Wait-Until -FailureMessage 'Timed out waiting for the visible ImgViewer window.' -Condition {
        $mainProcess.Refresh()
        if ($mainProcess.HasExited) {
            throw "ImgViewer exited during startup with code $($mainProcess.ExitCode)."
        }
        $candidate = [ImgViewerMemorySmokeNative]::FindMainWindow($mainProcess.Id)
        if ($candidate -eq [IntPtr]::Zero) {
            return $null
        }
        return $candidate
    }
    Wait-ViewerImage `
        -Window $window `
        -Name ([IO.Path]::GetFileName($helperFixturePath)) | Out-Null
    $initialSample = Get-ImgViewerProcessSample -RootProcessId $mainProcess.Id
    if ($initialSample.WebViewProcessCount -lt 1) {
        throw "No recursive msedgewebview2 descendant was found for ImgViewer PID $($mainProcess.Id)."
    }
    if ($initialSample.HelperProcessCount -ne 1) {
        throw (
            "Isolated-codec startup must create exactly one direct codec helper child; " +
            "found $($initialSample.HelperProcessCount)."
        )
    }
    $expectedHelperProcessId = [int](@($initialSample.HelperProcessIds)[0])
    $helperProcess = Get-Process -Id $expectedHelperProcessId -ErrorAction Stop
    $actualHelperPath = [IO.Path]::GetFullPath([string]$helperProcess.Path)
    if (-not [string]::Equals(
            $actualHelperPath,
            $helperExecutablePath,
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw (
            "Codec helper was not launched from the packaged sibling: " +
            "expected '$helperExecutablePath'; found '$actualHelperPath'."
        )
    }
    $helperProcessIds = @($initialSample.HelperProcessIds)
    $lastTreeProcessIds = @($initialSample.MeasuredProcessIds)

    for ($cycle = 1; $cycle -le $Cycles; $cycle++) {
        $image = $images[$cycle % $images.Count]
        $load = Open-ThroughExistingInstance `
            -ExecutablePath $executablePath `
            -ImagePath $image `
            -Window $window `
            -RootProcessId $mainProcess.Id
        if ($IdleMilliseconds -gt 0) {
            Start-Sleep -Milliseconds $IdleMilliseconds
        }

        $sample = Get-ImgViewerProcessSample -RootProcessId $mainProcess.Id
        $lastTreeProcessIds = @($sample.MeasuredProcessIds)
        if ($sample.WebViewProcessCount -lt 1) {
            throw "No recursive msedgewebview2 descendant was found for ImgViewer PID $($mainProcess.Id)."
        }
        if ($sample.HelperProcessCount -ne 1) {
            throw (
                "Expected one persistent direct codec helper at cycle $cycle; " +
                "found $($sample.HelperProcessCount)."
            )
        }
        $observedHelperProcessId = [int](@($sample.HelperProcessIds)[0])
        if ($observedHelperProcessId -ne $expectedHelperProcessId) {
            throw (
                "Codec helper PID changed during memory smoke at cycle ${cycle}: " +
                "expected $expectedHelperProcessId; found $observedHelperProcessId."
            )
        }
        $helperProcessIds = @(
            @($helperProcessIds) + @($sample.HelperProcessIds) |
                Sort-Object -Unique
        )
        $row = [pscustomobject]@{
            TimestampUtc = [DateTime]::UtcNow.ToString('o')
            Cycle = $cycle
            IsWarmup = $cycle -le $Warmup
            Image = [IO.Path]::GetFileName($image)
            LoadMilliseconds = $load.LoadMilliseconds
            WorkingSetSizeBytes = $sample.WorkingSetBytes
            PrivatePageCountBytes = $sample.PrivateBytes
            RootWorkingSetSizeBytes = $sample.RootWorkingSetBytes
            RootPrivatePageCountBytes = $sample.RootPrivateBytes
            WebViewWorkingSetSizeBytes = $sample.WebViewWorkingSetBytes
            WebViewPrivatePageCountBytes = $sample.WebViewPrivateBytes
            HelperWorkingSetSizeBytes = $sample.HelperWorkingSetBytes
            HelperPrivatePageCountBytes = $sample.HelperPrivateBytes
            PeakWorkingSetSizeBytes = $load.PeakWorkingSetBytes
            PeakPrivatePageCountBytes = $load.PeakPrivateBytes
            PeakRootWorkingSetSizeBytes = $load.PeakRootWorkingSetBytes
            PeakRootPrivatePageCountBytes = $load.PeakRootPrivateBytes
            PeakHelperWorkingSetSizeBytes = $load.PeakHelperWorkingSetBytes
            PeakHelperPrivatePageCountBytes = $load.PeakHelperPrivateBytes
            ProcessCount = $sample.ProcessCount
            WebViewProcessCount = $sample.WebViewProcessCount
            HelperProcessCount = $sample.HelperProcessCount
            HelperProcessId = $observedHelperProcessId
        }
        $rows.Add($row)
        if ($cycle -eq 1) {
            $row | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8
        }
        else {
            $row | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8 -Append
        }
    }

    $measured = @($rows | Where-Object { -not $_.IsWarmup })
    [double]$retainedPrivateMiB = Get-RetainedGrowthMiB -Samples $measured -Property 'PrivatePageCountBytes'
    [double]$retainedWorkingSetMiB = Get-RetainedGrowthMiB -Samples $measured -Property 'WorkingSetSizeBytes'
    [double]$privateSlope = Get-SlopeMiBPerCycle -Samples $measured -Property 'PrivatePageCountBytes'
    [double]$workingSetSlope = Get-SlopeMiBPerCycle -Samples $measured -Property 'WorkingSetSizeBytes'
    [double[]]$loadTimes = @($measured | ForEach-Object { [double]$_.LoadMilliseconds })
    [double]$p95LoadMilliseconds = Get-Percentile -Values $loadTimes -Percentile 0.95
    [double]$peakPrivateMiB = (
        $measured | Measure-Object -Property PeakPrivatePageCountBytes -Maximum
    ).Maximum / 1MB
    [double]$peakWorkingSetMiB = (
        $measured | Measure-Object -Property PeakWorkingSetSizeBytes -Maximum
    ).Maximum / 1MB
    [double]$peakRootPrivateMiB = (
        $measured | Measure-Object -Property PeakRootPrivatePageCountBytes -Maximum
    ).Maximum / 1MB
    [double]$peakRootWorkingSetMiB = (
        $measured | Measure-Object -Property PeakRootWorkingSetSizeBytes -Maximum
    ).Maximum / 1MB
    [double]$peakHelperPrivateMiB = (
        $measured | Measure-Object -Property PeakHelperPrivatePageCountBytes -Maximum
    ).Maximum / 1MB
    [double]$peakHelperWorkingSetMiB = (
        $measured | Measure-Object -Property PeakHelperWorkingSetSizeBytes -Maximum
    ).Maximum / 1MB

    $failures = [Collections.Generic.List[string]]::new()
    if ($retainedPrivateMiB -gt $MaxRetainedPrivateMiB) {
        $failures.Add("retained private memory $([Math]::Round($retainedPrivateMiB, 2)) MiB > $MaxRetainedPrivateMiB MiB")
    }
    if ($retainedWorkingSetMiB -gt $MaxRetainedWorkingSetMiB) {
        $failures.Add("retained working set $([Math]::Round($retainedWorkingSetMiB, 2)) MiB > $MaxRetainedWorkingSetMiB MiB")
    }
    if ($privateSlope -gt $MaxPrivateSlopeMiBPerCycle) {
        $failures.Add("private slope $([Math]::Round($privateSlope, 3)) MiB/cycle > $MaxPrivateSlopeMiBPerCycle MiB/cycle")
    }
    if ($workingSetSlope -gt $MaxWorkingSetSlopeMiBPerCycle) {
        $failures.Add("working-set slope $([Math]::Round($workingSetSlope, 3)) MiB/cycle > $MaxWorkingSetSlopeMiBPerCycle MiB/cycle")
    }
    if ($p95LoadMilliseconds -gt $MaxP95LoadMilliseconds) {
        $failures.Add("p95 load $([Math]::Round($p95LoadMilliseconds)) ms > $MaxP95LoadMilliseconds ms")
    }
    if ($failures.Count -gt 0) {
        throw "Memory smoke failed: $($failures -join '; '). CSV: $csvPath"
    }

    $finalSample = $rows[$rows.Count - 1]
    $summaryFormat = (
        'PASS memory-smoke cycles={0} warmup={1} images={2} size={3}x{4} ' +
        'retained-private-mib={5:F2} private-slope-mib-per-cycle={6:F3} ' +
        'retained-working-set-mib={7:F2} working-set-slope-mib-per-cycle={8:F3} ' +
        'peak-private-mib={9:F2} peak-working-set-mib={10:F2} ' +
        'peak-root-private-mib={11:F2} peak-root-working-set-mib={12:F2} ' +
        'peak-helper-private-mib={13:F2} peak-helper-working-set-mib={14:F2} ' +
        'p95-load-ms={15:F0} processes={16} webview-processes={17} ' +
        'helper-processes={18} helper-pid={19} helper-source={20} ' +
        'measurement=peak-poll-plus-uia-image-then-fixed-idle webdriver=absent csv="{21}"'
    )
    Write-Output ($summaryFormat -f
        $Cycles,
        $Warmup,
        $images.Count,
        $summaryWidth,
        $summaryHeight,
        $retainedPrivateMiB,
        $privateSlope,
        $retainedWorkingSetMiB,
        $workingSetSlope,
        $peakPrivateMiB,
        $peakWorkingSetMiB,
        $peakRootPrivateMiB,
        $peakRootWorkingSetMiB,
        $peakHelperPrivateMiB,
        $peakHelperWorkingSetMiB,
        $p95LoadMilliseconds,
        $finalSample.ProcessCount,
        $finalSample.WebViewProcessCount,
        $finalSample.HelperProcessCount,
        $finalSample.HelperProcessId,
        $helperSourceLabel,
        $csvPath
    )
    $memorySmokeCompleted = $true
}
finally {
    if ($null -ne $mainProcess) {
        try {
            $cleanupSample = Get-ImgViewerProcessSample -RootProcessId $mainProcess.Id
            $lastTreeProcessIds = @($cleanupSample.MeasuredProcessIds)
            $helperProcessIds = @(
                @($helperProcessIds) + @($cleanupSample.HelperProcessIds) |
                    Sort-Object -Unique
            )
        }
        catch {}
    }
    if ($window -ne [IntPtr]::Zero) {
        [ImgViewerMemorySmokeNative]::PostMessageW(
            $window,
            0x0010,
            [IntPtr]::Zero,
            [IntPtr]::Zero
        ) | Out-Null
    }
    if ($null -ne $mainProcess) {
        try {
            if (-not $mainProcess.WaitForExit(3000)) {
                $mainProcess.Kill()
                [void]$mainProcess.WaitForExit(2000)
            }
        }
        catch {}
        $mainProcess.Dispose()
    }
    foreach ($helperProcessId in @($helperProcessIds | Sort-Object -Unique)) {
        $helperExitDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while ((Test-NamedProcessRunning `
                -ProcessId ([int]$helperProcessId) `
                -ExpectedProcessName $helperProcessName) -and
            [DateTime]::UtcNow -lt $helperExitDeadline) {
            Start-Sleep -Milliseconds 50
        }
        if (Test-NamedProcessRunning `
                -ProcessId ([int]$helperProcessId) `
                -ExpectedProcessName $helperProcessName) {
            $helperCleanupFailure = (
                "Codec helper PID $helperProcessId remained alive after ImgViewer exited."
            )
            try {
                Stop-Process -Id ([int]$helperProcessId) -Force -ErrorAction SilentlyContinue
            }
            catch {}
        }
    }
    if (-not $helperCleanupFailure -and
        $null -ne $expectedHelperProcessId -and
        $memorySmokeCompleted) {
        Write-Output (
            "PASS memory-helper-cleanup direct-child=1 " +
            "persistent-pid=$expectedHelperProcessId orphan=absent webdriver=absent"
        )
    }
    $allowedCleanupNames = @(
        [IO.Path]::GetFileName($executablePath),
        'msedgewebview2.exe',
        [IO.Path]::GetFileName($helperExecutablePath)
    )
    $allowedProcessNames = @(
        $allowedCleanupNames |
            ForEach-Object { [IO.Path]::GetFileNameWithoutExtension($_) }
    )
    foreach ($processId in @($lastTreeProcessIds | Sort-Object -Descending -Unique)) {
        try {
            $candidate = Get-Process -Id ([int]$processId) -ErrorAction SilentlyContinue
            if ($null -ne $candidate -and $allowedProcessNames -contains $candidate.ProcessName) {
                Stop-Process -Id ([int]$processId) -Force -ErrorAction SilentlyContinue
            }
        }
        catch {}
    }

    $resolvedRunDirectory = [IO.Path]::GetFullPath($runDirectory)
    if ($resolvedRunDirectory.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($resolvedRunDirectory).StartsWith(
            'ImgViewer-memory-smoke-',
            [StringComparison]::Ordinal
        ) -and
        [IO.Directory]::Exists($resolvedRunDirectory)) {
        [IO.Directory]::Delete($resolvedRunDirectory, $true)
    }
    if ($helperCleanupFailure) {
        throw $helperCleanupFailure
    }
}
