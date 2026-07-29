[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [string]$FixtureDirectory,
    [int]$TimeoutSeconds = 15,
    [switch]$SkipPixelChecks,
    [switch]$UseAutomationInvoke,
    [switch]$UseNavigationClick,
    [switch]$ContinuityOnly,
    [switch]$HandoffFormatsOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Windows PowerShell 5.1 can evaluate parameter default expressions before
# $PSScriptRoot is populated. Resolve the repository-relative default only
# after parameter binding so direct `-File` invocations remain reliable.
if ([string]::IsNullOrWhiteSpace($FixtureDirectory)) {
    $FixtureDirectory = Join-Path $PSScriptRoot '..\tests\fixtures'
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class ImgViewerNativeSmoke
{
    public sealed class ProcessIdentity
    {
        public uint ProcessId { get; set; }
        public uint ParentProcessId { get; set; }
        public string Name { get; set; }
    }

    public delegate bool EnumWindowsCallback(IntPtr hwnd, IntPtr state);

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
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

    private static readonly IntPtr InvalidHandleValue = new IntPtr(-1);
    private const uint SnapshotProcesses = 0x00000002;

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr state);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int capacity);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(
        IntPtr hwnd,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );

    [DllImport("user32.dll")]
    public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    private static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool Process32FirstW(IntPtr snapshot, ref ProcessEntry32 entry);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool Process32NextW(IntPtr snapshot, ref ProcessEntry32 entry);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    public static void LeftClick(int x, int y)
    {
        if (!SetCursorPos(x, y))
            throw new InvalidOperationException("SetCursorPos failed.");
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }

    public static void MakeTopmost(IntPtr hwnd)
    {
        // Keep the smoke target visible for CopyFromScreen without moving,
        // resizing, or activating it. The process is closed at the end.
        if (!SetWindowPos(hwnd, new IntPtr(-1), 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0010 | 0x0040))
            throw new InvalidOperationException("SetWindowPos(HWND_TOPMOST) failed.");
    }

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

    public static Rect ReadRect(IntPtr hwnd)
    {
        Rect rect;
        if (!GetWindowRect(hwnd, out rect))
            throw new InvalidOperationException("GetWindowRect failed.");
        return rect;
    }

    public static ProcessIdentity[] ReadDirectChildren(uint expectedParentProcessId)
    {
        IntPtr snapshot = CreateToolhelp32Snapshot(SnapshotProcesses, 0);
        if (snapshot == InvalidHandleValue)
            throw new InvalidOperationException(
                "CreateToolhelp32Snapshot failed with Win32 error " +
                Marshal.GetLastWin32Error() + "."
            );

        var result = new List<ProcessIdentity>();
        try
        {
            var entry = new ProcessEntry32();
            entry.Size = (uint)Marshal.SizeOf(typeof(ProcessEntry32));
            if (!Process32FirstW(snapshot, ref entry))
                throw new InvalidOperationException(
                    "Process32FirstW failed with Win32 error " +
                    Marshal.GetLastWin32Error() + "."
                );

            do
            {
                if (entry.ParentProcessId == expectedParentProcessId)
                {
                    result.Add(new ProcessIdentity
                    {
                        ProcessId = entry.ProcessId,
                        ParentProcessId = entry.ParentProcessId,
                        Name = entry.ExeFile
                    });
                }
                entry.Size = (uint)Marshal.SizeOf(typeof(ProcessEntry32));
            }
            while (Process32NextW(snapshot, ref entry));
        }
        finally
        {
            CloseHandle(snapshot);
        }
        return result.ToArray();
    }
}
'@

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [string]$FailureMessage
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $value = & $Condition
        if ($null -ne $value -and $value -ne $false) { return $value }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $FailureMessage
}

function Get-DirectCodecHelpers {
    param([Parameter(Mandatory = $true)][int]$RootProcessId)

    return @(
        [ImgViewerNativeSmoke]::ReadDirectChildren([uint32]$RootProcessId) |
            Where-Object {
                [string]::Equals(
                    [string]$_.Name,
                    'ImgViewer.CodecHelper.exe',
                    [StringComparison]::OrdinalIgnoreCase
                )
            }
    )
}

function Wait-SingleCodecHelper {
    param([Parameter(Mandatory = $true)][int]$RootProcessId)

    return Wait-Until `
        -FailureMessage "Timed out waiting for the unique direct codec helper child of ImgViewer PID $RootProcessId." `
        -Condition {
            $helpers = @(Get-DirectCodecHelpers -RootProcessId $RootProcessId)
            if ($helpers.Count -gt 1) {
                $ids = @($helpers | ForEach-Object { $_.ProcessId })
                throw "ImgViewer PID $RootProcessId has multiple direct codec helpers: $($ids -join ', ')."
            }
            if ($helpers.Count -eq 1) {
                return $helpers[0]
            }
            return $null
        }
}

function Assert-PersistentCodecHelper {
    param(
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [Parameter(Mandatory = $true)][int]$ExpectedProcessId
    )

    $helper = Wait-SingleCodecHelper -RootProcessId $RootProcessId
    if ([int]$helper.ProcessId -ne $ExpectedProcessId) {
        throw (
            "Codec helper was replaced between HEIC and HEIF decode: " +
            "expected PID $ExpectedProcessId; found PID $($helper.ProcessId)."
        )
    }
    return $helper
}

function Test-CodecHelperProcessRunning {
    param([Parameter(Mandatory = $true)][int]$ProcessId)

    $candidate = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    return (
        $null -ne $candidate -and
        [string]::Equals(
            [string]$candidate.ProcessName,
            'ImgViewer.CodecHelper',
            [StringComparison]::OrdinalIgnoreCase
        )
    )
}

function Assert-CodecHelperExecutablePath {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string]$ExpectedPath
    )

    $candidate = Get-Process -Id $ProcessId -ErrorAction Stop
    $actualPath = [IO.Path]::GetFullPath([string]$candidate.Path)
    $expectedFullPath = [IO.Path]::GetFullPath($ExpectedPath)
    if (-not [string]::Equals(
            $actualPath,
            $expectedFullPath,
            [StringComparison]::OrdinalIgnoreCase
        )) {
        throw (
            "Codec helper PID $ProcessId was not launched from the packaged sibling: " +
            "expected '$expectedFullPath'; found '$actualPath'."
        )
    }
}

function Wait-Image {
    param([IntPtr]$Window, [string]$Name)
    return Wait-Until -FailureMessage "Timed out waiting for rendered image '$Name'." -Condition {
        try {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
            if ($null -eq $root) { return $null }
            $nameCondition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty,
                $Name
            )
            $typeCondition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::Image
            )
            $condition = [System.Windows.Automation.AndCondition]::new($nameCondition, $typeCondition)
            return $root.FindFirst(
                [System.Windows.Automation.TreeScope]::Descendants,
                $condition
            )
        }
        catch {
            # WebView2 can replace its child HWND while the native window is
            # already visible. Treat that short UIA gap as not-ready.
            return $null
        }
    }
}

function Get-ViewerImages {
    param([IntPtr]$Window)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
    if ($null -eq $root) { return }
    $typeCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Image
    )
    $elements = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        $typeCondition
    )
    for ($index = 0; $index -lt $elements.Count; $index++) {
        Write-Output $elements.Item($index)
    }
}

function Wait-Text {
    param([IntPtr]$Window, [string]$Name)
    return Wait-Until -FailureMessage "Timed out waiting for UI text '$Name'." -Condition {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
        if ($null -eq $root) { return $null }
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            $Name
        )
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
}

function Wait-Control {
    param(
        [IntPtr]$Window,
        [string]$Name,
        [System.Windows.Automation.ControlType]$ControlType
    )
    return Wait-Until -FailureMessage "Timed out waiting for UI control '$Name'." -Condition {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
        if ($null -eq $root) { return $null }
        $nameCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            $Name
        )
        $typeCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            $ControlType
        )
        $root.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.AndCondition]::new($nameCondition, $typeCondition)
        )
    }
}

function Read-ZoomPercent {
    param([IntPtr]$Window)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
    if ($null -eq $root) { return $null }
    $typeCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text
    )
    $elements = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        $typeCondition
    )
    foreach ($element in $elements) {
        $name = $element.Current.Name
        if ($name -match '^\d+%$') { return $name }
    }
    return $null
}

function Wait-ZoomPercent {
    param([IntPtr]$Window, [string]$Expected)
    return Wait-Until -FailureMessage "Timed out waiting for zoom '$Expected'; current zoom is '$(Read-ZoomPercent $Window)'." -Condition {
        $current = Read-ZoomPercent $Window
        if ($current -eq $Expected) { return $current }
        return $null
    }
}

function Click-ViewerButton {
    param(
        [IntPtr]$Window,
        [string]$Name,
        [switch]$NoWait
    )
    $button = Wait-Control $Window $Name ([System.Windows.Automation.ControlType]::Button)
    if ($UseAutomationInvoke) {
        $invokePattern = $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
        $invokePattern.Invoke()
        if (-not $NoWait) { Start-Sleep -Milliseconds 150 }
        return
    }

    [ImgViewerNativeSmoke]::MakeTopmost($Window)
    [ImgViewerNativeSmoke]::SetForegroundWindow($Window) | Out-Null
    $bounds = $button.Current.BoundingRectangle
    if ($bounds.Width -lt 1 -or $bounds.Height -lt 1) {
        throw "Viewer button '$Name' has an empty UI Automation rectangle: $bounds"
    }
    $x = [int][Math]::Floor($bounds.Left + $bounds.Width / 2)
    $y = [int][Math]::Floor($bounds.Top + $bounds.Height / 2)
    [ImgViewerNativeSmoke]::LeftClick($x, $y)
    if (-not $NoWait) { Start-Sleep -Milliseconds 150 }
}

function Read-ImageCenterPixel {
    param([System.Windows.Automation.AutomationElement]$Image)
    $bounds = $Image.Current.BoundingRectangle
    if ($bounds.Width -lt 1 -or $bounds.Height -lt 1) {
        throw "Rendered image has an empty UI Automation rectangle: $bounds"
    }
    $x = [int][Math]::Floor($bounds.Left + $bounds.Width / 2)
    $y = [int][Math]::Floor($bounds.Top + $bounds.Height / 2)
    $bitmap = New-Object System.Drawing.Bitmap 1, 1
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($x, $y, 0, 0, [System.Drawing.Size]::new(1, 1))
        return $bitmap.GetPixel(0, 0)
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Assert-DominantColor {
    param($Color, [ValidateSet('red', 'green', 'blue')] [string]$Expected, [string]$Context)
    $channels = @{
        red = [int]$Color.R
        green = [int]$Color.G
        blue = [int]$Color.B
    }
    $others = @($channels.Keys | Where-Object { $_ -ne $Expected } | ForEach-Object { $channels[$_] })
    if ($channels[$Expected] -lt 120 -or $channels[$Expected] -lt (($others | Measure-Object -Maximum).Maximum + 60)) {
        throw "$Context rendered the wrong center pixel: expected $Expected dominance; got R=$($Color.R), G=$($Color.G), B=$($Color.B)."
    }
}

function Wait-DominantColor {
    param(
        [IntPtr]$Window,
        [System.Windows.Automation.AutomationElement]$Image,
        [ValidateSet('red', 'green', 'blue')] [string]$Expected,
        [string]$Context
    )
    [ImgViewerNativeSmoke]::MakeTopmost($Window)
    # SetForegroundWindow may report false when Windows has already activated the
    # freshly launched window. Pixel readiness is the authoritative check here.
    [ImgViewerNativeSmoke]::SetForegroundWindow($Window) | Out-Null

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastColor = $null
    do {
        $lastColor = Read-ImageCenterPixel $Image
        $channels = @{
            red = [int]$lastColor.R
            green = [int]$lastColor.G
            blue = [int]$lastColor.B
        }
        $otherMaximum = @(
            $channels.Keys |
                Where-Object { $_ -ne $Expected } |
                ForEach-Object { $channels[$_] }
        ) | Measure-Object -Maximum
        if ($channels[$Expected] -ge 120 -and
            $channels[$Expected] -ge ($otherMaximum.Maximum + 60)) {
            return $lastColor
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)

    Assert-DominantColor $lastColor $Expected $Context
}

function Get-DominantPrimaryColor {
    param($Color)
    $channels = @{
        red = [int]$Color.R
        green = [int]$Color.G
        blue = [int]$Color.B
    }
    foreach ($candidate in @('red', 'green', 'blue')) {
        $others = @(
            $channels.Keys |
                Where-Object { $_ -ne $candidate } |
                ForEach-Object { $channels[$_] }
        )
        $otherMaximum = ($others | Measure-Object -Maximum).Maximum
        if ($channels[$candidate] -ge 120 -and
            $channels[$candidate] -ge ($otherMaximum + 60)) {
            return $candidate
        }
    }
    return $null
}

function Open-ThroughExistingInstance {
    param([string]$Path, [IntPtr]$Window)
    $second = Start-Process -FilePath $executablePath `
        -ArgumentList ('"' + $Path + '"') `
        -PassThru
    try {
        if (-not $second.WaitForExit($TimeoutSeconds * 1000)) {
            try { $second.Kill() } catch {}
            throw "Second instance did not hand off '$Path' and exit."
        }
        if ($second.ExitCode -ne 0) {
            throw "Second instance handoff for '$Path' exited with code $($second.ExitCode)."
        }
    }
    finally {
        $second.Dispose()
    }
    return Wait-Image $Window ([IO.Path]::GetFileName($Path))
}

function Assert-AnimatedPixels {
    param([IntPtr]$Window, [string]$Name)
    $colors = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    for ($sample = 0; $sample -lt 10; $sample++) {
        $image = Wait-Image $Window $Name
        $pixel = Read-ImageCenterPixel $image
        [void]$colors.Add("$($pixel.R),$($pixel.G),$($pixel.B),$($pixel.A)")
        Start-Sleep -Milliseconds 90
    }
    if ($colors.Count -lt 2) {
        throw "$Name did not visibly advance frames; sampled colors: $($colors -join '; ')."
    }
    Write-Output "PASS animation file=$Name sampled-colors=$($colors.Count)"
}

function Send-ViewerKeys {
    param([IntPtr]$Window, [string]$Keys)
    [ImgViewerNativeSmoke]::MakeTopmost($Window)
    $rect = [ImgViewerNativeSmoke]::ReadRect($Window)
    # Focus the WebView content instead of repeatedly clicking the title bar.
    # Two title-bar clicks inside the Windows double-click interval maximize
    # the window and make the geometry assertion fail for the wrong reason.
    [ImgViewerNativeSmoke]::LeftClick(
        [int][Math]::Floor(($rect.Left + $rect.Right) / 2),
        [int][Math]::Floor(($rect.Top + $rect.Bottom) / 2)
    )
    [ImgViewerNativeSmoke]::SetForegroundWindow($Window) | Out-Null
    Start-Sleep -Milliseconds 100
    [System.Windows.Forms.SendKeys]::SendWait($Keys)
}

function Navigate-Viewer {
    param(
        [IntPtr]$Window,
        [ValidateSet('previous', 'next')] [string]$Direction,
        [int]$Count = 1
    )
    if ($UseNavigationClick) {
        $buttonName = if ($Direction -eq 'previous') {
            ([char[]](0x4E0A, 0x4E00, 0x5F35) -join '')
        }
        else {
            ([char[]](0x4E0B, 0x4E00, 0x5F35) -join '')
        }
        for ($step = 0; $step -lt $Count; $step++) {
            Click-ViewerButton $Window $buttonName
        }
        return
    }

    if (-not $UseAutomationInvoke) {
        $key = if ($Direction -eq 'previous') { '{LEFT}' } else { '{RIGHT}' }
        Send-ViewerKeys $Window (($key * $Count) -join '')
        return
    }

    $buttonName = if ($Direction -eq 'previous') {
        ([char[]](0x4E0A, 0x4E00, 0x5F35) -join '')
    }
    else {
        ([char[]](0x4E0B, 0x4E00, 0x5F35) -join '')
    }
    for ($step = 0; $step -lt $Count; $step++) {
        $button = Wait-Control $Window $buttonName ([System.Windows.Automation.ControlType]::Button)
        if (-not $button.Current.IsEnabled) {
            return
        }
        $invokePattern = $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
        $invokePattern.Invoke()
        Start-Sleep -Milliseconds 100
    }
}

function Begin-ViewerNavigation {
    param(
        [IntPtr]$Window,
        [ValidateSet('previous', 'next')] [string]$Direction
    )
    if ($UseNavigationClick) {
        $buttonName = if ($Direction -eq 'previous') {
            ([char[]](0x4E0A, 0x4E00, 0x5F35) -join '')
        }
        else {
            ([char[]](0x4E0B, 0x4E00, 0x5F35) -join '')
        }
        Click-ViewerButton $Window $buttonName -NoWait
        return
    }

    if (-not $UseAutomationInvoke) {
        $key = if ($Direction -eq 'previous') { '{LEFT}' } else { '{RIGHT}' }
        Send-ViewerKeys $Window $key
        return
    }

    $buttonName = if ($Direction -eq 'previous') {
        ([char[]](0x4E0A, 0x4E00, 0x5F35) -join '')
    }
    else {
        ([char[]](0x4E0B, 0x4E00, 0x5F35) -join '')
    }
    $button = Wait-Control $Window $buttonName ([System.Windows.Automation.ControlType]::Button)
    if (-not $button.Current.IsEnabled) {
        throw "Viewer button '$buttonName' was disabled before the continuity navigation."
    }
    $invokePattern = $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invokePattern.Invoke()
}

function Assert-SwitchContinuity {
    param(
        [IntPtr]$Window,
        [string]$OldName,
        [string]$NewName,
        [ValidateSet('red', 'green', 'blue')] [string]$OldColor,
        [ValidateSet('red', 'green', 'blue')] [string]$NewColor,
        [string]$HandoffPath
    )
    [ImgViewerNativeSmoke]::MakeTopmost($Window)
    [ImgViewerNativeSmoke]::SetForegroundWindow($Window) | Out-Null

    $before = @(Get-ViewerImages $Window)
    if ($before.Count -lt 1) {
        throw "Continuity precondition failed: no UI Automation Image was present for '$OldName'."
    }

    $handoffProcess = $null
    try {
        if ([string]::IsNullOrWhiteSpace($HandoffPath)) {
            Begin-ViewerNavigation $Window 'next'
        }
        else {
            $handoffProcess = Start-Process -FilePath $executablePath `
                -ArgumentList ('"' + $HandoffPath + '"') `
                -PassThru
        }

        $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $minimumImageCount = [int]::MaxValue
        $samples = 0
        $lastPixel = $null
        $lastSampleError = $null
        do {
            $images = @(Get-ViewerImages $Window)
            $samples++
            if ($images.Count -lt $minimumImageCount) {
                $minimumImageCount = $images.Count
            }
            if ($images.Count -eq 0) {
                throw "Image switch flashed blank: UI Automation Image count reached 0 while changing '$OldName' to '$NewName' (sample=$samples)."
            }

            $newImage = $null
            foreach ($image in $images) {
                try {
                    if ($image.Current.Name -eq $NewName) {
                        $newImage = $image
                        break
                    }
                }
                catch {
                    # The DOM may replace an AutomationElement between FindAll
                    # and reading Current. Re-sample immediately; a real gap is
                    # still caught by the zero-count assertion on the next pass.
                    $lastSampleError = $_.Exception.Message
                }
            }

            if ($SkipPixelChecks) {
                if ($null -ne $newImage) {
                    return [pscustomobject]@{
                        MinimumImageCount = $minimumImageCount
                        Samples = $samples
                        Pixel = 'skipped'
                    }
                }
            }
            else {
                $sampleImage = if ($null -ne $newImage) { $newImage } else { $images[0] }
                try {
                    $lastPixel = Read-ImageCenterPixel $sampleImage
                    $dominant = Get-DominantPrimaryColor $lastPixel
                    if ($dominant -ne $OldColor -and $dominant -ne $NewColor) {
                        throw "Image switch rendered an intermediate center pixel while changing '$OldName' to '$NewName': expected $OldColor or $NewColor; got R=$($lastPixel.R), G=$($lastPixel.G), B=$($lastPixel.B) (sample=$samples)."
                    }
                    if ($null -ne $newImage -and $dominant -eq $NewColor) {
                        return [pscustomobject]@{
                            MinimumImageCount = $minimumImageCount
                            Samples = $samples
                            Pixel = 'old-or-new'
                        }
                    }
                }
                catch {
                    if ($_.Exception.Message -like 'Image switch rendered an intermediate*') {
                        throw
                    }
                    $lastSampleError = $_.Exception.Message
                    $afterFailure = @(Get-ViewerImages $Window)
                    if ($afterFailure.Count -eq 0) {
                        throw "Image switch flashed blank after a stale UI Automation element while changing '$OldName' to '$NewName' (sample=$samples)."
                    }
                }
            }

            Start-Sleep -Milliseconds 10
        } while ([DateTime]::UtcNow -lt $deadline)

        if ($SkipPixelChecks) {
            throw "Timed out waiting for '$NewName' during continuity sampling; minimum UI Automation Image count=$minimumImageCount, samples=$samples."
        }
        $pixelText = if ($null -eq $lastPixel) {
            'unavailable'
        }
        else {
            "R=$($lastPixel.R),G=$($lastPixel.G),B=$($lastPixel.B)"
        }
        throw "Timed out waiting for '$NewName' to paint $NewColor during continuity sampling; minimum UI Automation Image count=$minimumImageCount, samples=$samples, last-pixel=$pixelText, last-error=$lastSampleError."
    }
    finally {
        if ($null -ne $handoffProcess) {
            try {
                if (-not $handoffProcess.WaitForExit($TimeoutSeconds * 1000)) {
                    $handoffProcess.Kill()
                }
            }
            catch {}
            $handoffProcess.Dispose()
        }
    }
}

function Format-Rect {
    param($Rect)
    return "$($Rect.Left),$($Rect.Top),$($Rect.Right),$($Rect.Bottom)"
}

function Assert-SameRect {
    param($Expected, $Actual, [string]$Context)
    $expectedText = Format-Rect $Expected
    $actualText = Format-Rect $Actual
    if ($expectedText -ne $actualText) {
        throw "$Context changed the native window rect: expected $expectedText; actual $actualText."
    }
}

$executablePath = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $Executable).Path)
$helperExecutablePath = Join-Path (
    [IO.Path]::GetDirectoryName($executablePath)
) 'ImgViewer.CodecHelper.exe'
if (-not [IO.File]::Exists($helperExecutablePath)) {
    throw "Packaged codec helper is missing beside ImgViewer.exe: $helperExecutablePath"
}
$fixturePath = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $FixtureDirectory).Path)
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$smokeDirectory = Join-Path $tempRoot ("ImgViewer-native-smoke-" + [Guid]::NewGuid().ToString('N'))
$process = $null
$window = [IntPtr]::Zero
$helperProcessId = $null
$helperProcessIds = @()
$helperPersistenceVerified = $false
$helperCleanupFailure = $null
$nativeSmokeCompleted = $false

try {
    [IO.Directory]::CreateDirectory($smokeDirectory) | Out-Null
    $sequenceDirectory = Join-Path $smokeDirectory 'sequence'
    $formatDirectory = Join-Path $smokeDirectory 'formats'
    $continuityDirectory = Join-Path $smokeDirectory 'continuity'
    [IO.Directory]::CreateDirectory($sequenceDirectory) | Out-Null
    [IO.Directory]::CreateDirectory($formatDirectory) | Out-Null
    [IO.Directory]::CreateDirectory($continuityDirectory) | Out-Null
    foreach ($name in @('1.jpg', '2.jpg', '10.jpg')) {
        [IO.File]::Copy((Join-Path $fixturePath $name), (Join-Path $sequenceDirectory $name), $false)
    }
    foreach ($name in @(
        'animated.gif',
        'animated.webp',
        'transparent.png',
        'two-page.tiff',
        'primary-second.heic',
        'single.heif',
        'corrupt.jpg'
    )) {
        [IO.File]::Copy((Join-Path $fixturePath $name), (Join-Path $formatDirectory $name), $false)
    }
    $zoomImagePath = Join-Path $formatDirectory 'zoom-large.png'
    # Make one side wider than the entire virtual desktop while keeping the
    # pixel count small. This guarantees an initial Fit below 100% even when a
    # prior run restored the viewer maximized on a large monitor.
    $zoomWidth = [Math]::Min(
        32000,
        [System.Windows.Forms.SystemInformation]::VirtualScreen.Width + 400
    )
    $zoomBitmap = New-Object System.Drawing.Bitmap $zoomWidth, 100
    $zoomGraphics = [System.Drawing.Graphics]::FromImage($zoomBitmap)
    try {
        $zoomGraphics.Clear([System.Drawing.Color]::FromArgb(255, 48, 112, 196))
        $zoomBitmap.Save($zoomImagePath, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $zoomGraphics.Dispose()
        $zoomBitmap.Dispose()
    }

    $continuityRedPath = Join-Path $continuityDirectory '1-continuity-red.png'
    $continuityGreenPath = Join-Path $continuityDirectory '2-continuity-green.tiff'
    $continuityRedBitmap = [System.Drawing.Bitmap]::new(
        512,
        512,
        [System.Drawing.Imaging.PixelFormat]::Format24bppRgb
    )
    $continuityRedGraphics = [System.Drawing.Graphics]::FromImage($continuityRedBitmap)
    try {
        $continuityRedGraphics.Clear([System.Drawing.Color]::Red)
        $continuityRedBitmap.Save($continuityRedPath, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $continuityRedGraphics.Dispose()
        $continuityRedBitmap.Dispose()
    }

    # TIFF is intentionally much larger than the old PNG. Its native decode
    # and PNG conversion leave enough time for the high-frequency UIA sampler
    # to catch a loading-state regression without WebDriver or WDIO.
    $continuityGreenBitmap = [System.Drawing.Bitmap]::new(
        6000,
        4000,
        [System.Drawing.Imaging.PixelFormat]::Format24bppRgb
    )
    $continuityGreenGraphics = [System.Drawing.Graphics]::FromImage($continuityGreenBitmap)
    try {
        $continuityGreenGraphics.Clear([System.Drawing.Color]::Lime)
        $continuityGreenBitmap.Save($continuityGreenPath, [System.Drawing.Imaging.ImageFormat]::Tiff)
    }
    finally {
        $continuityGreenGraphics.Dispose()
        $continuityGreenBitmap.Dispose()
    }

    $initialPath = if ($ContinuityOnly) {
        $continuityRedPath
    }
    else {
        Join-Path $sequenceDirectory '2.jpg'
    }
    $process = Start-Process -FilePath $executablePath `
        -ArgumentList ('"' + $initialPath + '"') `
        -PassThru
    $window = Wait-Until -FailureMessage 'Timed out waiting for the visible ImgViewer window.' -Condition {
        $process.Refresh()
        if ($process.HasExited) { throw "ImgViewer exited during startup with code $($process.ExitCode)." }
        $candidate = [ImgViewerNativeSmoke]::FindMainWindow($process.Id)
        if ($candidate -eq [IntPtr]::Zero) { return $null }
        return $candidate
    }
    [ImgViewerNativeSmoke]::MakeTopmost($window)

    if ($ContinuityOnly) {
        $continuityOldImage = Wait-Image $window ([IO.Path]::GetFileName($continuityRedPath))
        $baseline = [ImgViewerNativeSmoke]::ReadRect($window)
        if (-not $SkipPixelChecks) {
            Wait-DominantColor $window $continuityOldImage 'red' 'continuity old image' | Out-Null
        }
        $continuity = Assert-SwitchContinuity `
            $window `
            ([IO.Path]::GetFileName($continuityRedPath)) `
            ([IO.Path]::GetFileName($continuityGreenPath)) `
            'red' `
            'green' `
            $continuityGreenPath
        Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'switch continuity handoff'
        $continuityFinal = if ($SkipPixelChecks) { 'target-uia' } else { 'green' }
        Write-Output "PASS switch-continuity old=$([IO.Path]::GetFileName($continuityRedPath)) new=$([IO.Path]::GetFileName($continuityGreenPath)) uia-image-min=$($continuity.MinimumImageCount) pixel=$($continuity.Pixel) final=$continuityFinal samples=$($continuity.Samples) trigger=single-instance-handoff rect=unchanged webdriver=absent"
        Write-Output 'PASS continuity-native-smoke continuity=1 trigger=single-instance-handoff webdriver=absent'
        return
    }

    $initialImage = Wait-Image $window '2.jpg'
    $baseline = [ImgViewerNativeSmoke]::ReadRect($window)
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $initialImage 'green' 'CLI open' | Out-Null
    }
    Write-Output "PASS cli-open image=2.jpg rect=$(Format-Rect $baseline)"

    if ($HandoffFormatsOnly) {
        foreach ($case in @(
            @{ Name = 'transparent.png'; Format = 'PNG' },
            @{ Name = 'animated.gif'; Format = 'GIF' },
            @{ Name = 'animated.webp'; Format = 'WebP' },
            @{ Name = 'two-page.tiff'; Format = 'TIFF' },
            @{ Name = 'primary-second.heic'; Format = 'HEIC' },
            @{ Name = 'single.heif'; Format = 'HEIF' }
        )) {
            Open-ThroughExistingInstance (Join-Path $formatDirectory $case.Name) $window | Out-Null
            if ($case.Format -eq 'HEIC') {
                $helper = Wait-SingleCodecHelper -RootProcessId $process.Id
                $helperProcessId = [int]$helper.ProcessId
                $helperProcessIds = @($helperProcessId)
                Assert-CodecHelperExecutablePath `
                    -ProcessId $helperProcessId `
                    -ExpectedPath $helperExecutablePath
            }
            elseif ($case.Format -eq 'HEIF') {
                if ($null -eq $helperProcessId) {
                    throw 'HEIF decoded before the codec helper PID was captured.'
                }
                Assert-PersistentCodecHelper `
                    -RootProcessId $process.Id `
                    -ExpectedProcessId $helperProcessId | Out-Null
                $helperPersistenceVerified = $true
            }
            Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) "$($case.Format) handoff"
            Write-Output "PASS format=$($case.Format) file=$($case.Name) rect=unchanged"
        }
        if (-not $helperPersistenceVerified) {
            throw 'The HEIC-to-HEIF codec helper persistence check did not run.'
        }
        $nativeSmokeCompleted = $true
        Write-Output 'PASS handoff-format-smoke formats=7 animations-opened=2 rect=unchanged webdriver=absent'
        return
    }

    Navigate-Viewer $window 'next'
    $nextImage = Wait-Image $window '10.jpg'
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $nextImage 'blue' 'natural-sort navigation' | Out-Null
    }
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'next navigation'
    Write-Output 'PASS natural-sort sequence=2.jpg->10.jpg rect=unchanged'

    Navigate-Viewer $window 'next'
    Start-Sleep -Milliseconds 400
    $endImage = Wait-Image $window '10.jpg'
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $endImage 'blue' 'end-stop navigation' | Out-Null
    }
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'end-stop navigation'
    Write-Output 'PASS end-stop file=10.jpg rect=unchanged'

    Navigate-Viewer $window 'previous' 2
    Wait-Image $window '1.jpg' | Out-Null
    Navigate-Viewer $window 'next' 2
    $rapidImage = Wait-Image $window '10.jpg'
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $rapidImage 'blue' 'rapid latest-wins navigation' | Out-Null
    }
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'rapid latest-wins navigation'
    Write-Output 'PASS rapid-navigation final=10.jpg rect=unchanged'

    $continuityOldImage = Open-ThroughExistingInstance $continuityRedPath $window
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $continuityOldImage 'red' 'continuity old image' | Out-Null
    }
    $continuity = Assert-SwitchContinuity `
        $window `
        ([IO.Path]::GetFileName($continuityRedPath)) `
        ([IO.Path]::GetFileName($continuityGreenPath)) `
        'red' `
        'green' `
        $continuityGreenPath
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'switch continuity handoff'
    $continuityFinal = if ($SkipPixelChecks) { 'target-uia' } else { 'green' }
    Write-Output "PASS switch-continuity old=$([IO.Path]::GetFileName($continuityRedPath)) new=$([IO.Path]::GetFileName($continuityGreenPath)) uia-image-min=$($continuity.MinimumImageCount) pixel=$($continuity.Pixel) final=$continuityFinal samples=$($continuity.Samples) trigger=single-instance-handoff rect=unchanged webdriver=absent"

    foreach ($case in @(
        @{ Name = 'transparent.png'; Format = 'PNG' },
        @{ Name = 'animated.gif'; Format = 'GIF' },
        @{ Name = 'animated.webp'; Format = 'WebP' },
        @{ Name = 'two-page.tiff'; Format = 'TIFF' },
        @{ Name = 'primary-second.heic'; Format = 'HEIC' },
        @{ Name = 'single.heif'; Format = 'HEIF' }
    )) {
        $image = Open-ThroughExistingInstance (Join-Path $formatDirectory $case.Name) $window
        if ($case.Format -eq 'HEIC') {
            $helper = Wait-SingleCodecHelper -RootProcessId $process.Id
            $helperProcessId = [int]$helper.ProcessId
            $helperProcessIds = @($helperProcessId)
            Assert-CodecHelperExecutablePath `
                -ProcessId $helperProcessId `
                -ExpectedPath $helperExecutablePath
        }
        elseif ($case.Format -eq 'HEIF') {
            if ($null -eq $helperProcessId) {
                throw 'HEIF decoded before the codec helper PID was captured.'
            }
            Assert-PersistentCodecHelper `
                -RootProcessId $process.Id `
                -ExpectedProcessId $helperProcessId | Out-Null
            $helperPersistenceVerified = $true
        }
        Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) "$($case.Format) handoff"
        Write-Output "PASS format=$($case.Format) file=$($case.Name) rect=unchanged"
    }
    if (-not $helperPersistenceVerified) {
        throw 'The HEIC-to-HEIF codec helper persistence check did not run.'
    }

    Open-ThroughExistingInstance (Join-Path $formatDirectory 'animated.gif') $window | Out-Null
    if (-not $SkipPixelChecks) {
        Assert-AnimatedPixels $window 'animated.gif'
    }
    Open-ThroughExistingInstance (Join-Path $formatDirectory 'animated.webp') $window | Out-Null
    if (-not $SkipPixelChecks) {
        Assert-AnimatedPixels $window 'animated.webp'
    }

    $errorProcess = Start-Process -FilePath $executablePath `
        -ArgumentList ('"' + (Join-Path $formatDirectory 'corrupt.jpg') + '"') `
        -PassThru
    try {
        if (-not $errorProcess.WaitForExit($TimeoutSeconds * 1000)) {
            try { $errorProcess.Kill() } catch {}
            throw 'Corrupt-image handoff did not exit.'
        }
    }
    finally {
        $errorProcess.Dispose()
    }
    Wait-Text $window '無法顯示圖片' | Out-Null
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'corrupt-image error'
    Navigate-Viewer $window 'next'
    Wait-Image $window 'primary-second.heic' | Out-Null
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'error recovery navigation'
    Write-Output 'PASS recoverable-error file=corrupt.jpg next=primary-second.heic rect=unchanged'

    $handoffImage = Open-ThroughExistingInstance (Join-Path $sequenceDirectory '1.jpg') $window
    if (-not $SkipPixelChecks) {
        Wait-DominantColor $window $handoffImage 'red' 'single-instance handoff' | Out-Null
    }
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'single-instance handoff'
    Write-Output 'PASS single-instance handoff=1.jpg rect=unchanged'

    Open-ThroughExistingInstance $zoomImagePath $window | Out-Null
    $fitZoom = Wait-Until -FailureMessage 'Timed out waiting for the initial Fit zoom.' -Condition {
        $value = Read-ZoomPercent $window
        if ($value -and $value -ne '100%') { return $value }
        return $null
    }
    Click-ViewerButton $window '100%'
    Wait-ZoomPercent $window '100%' | Out-Null
    Click-ViewerButton $window '放大'
    Wait-ZoomPercent $window '125%' | Out-Null
    Click-ViewerButton $window '放大'
    Wait-ZoomPercent $window '156%' | Out-Null
    Click-ViewerButton $window '縮小'
    Wait-ZoomPercent $window '125%' | Out-Null
    Click-ViewerButton $window '符合視窗'
    Wait-ZoomPercent $window $fitZoom | Out-Null
    Assert-SameRect $baseline ([ImgViewerNativeSmoke]::ReadRect($window)) 'zoom controls'
    $zoomInput = if ($UseAutomationInvoke) { 'uia-invoke' } else { 'mouse' }
    Write-Output "PASS zoom-controls sequence=$fitZoom->100%->125%->156%->125%->$fitZoom rect=unchanged input=$zoomInput"

    if ($SkipPixelChecks) {
        Write-Output "PASS native-ui-smoke formats=7 animations-opened=2 navigation=4 continuity=1 error-recovery=1 pixel-checks=skipped input=$zoomInput webdriver=absent"
    }
    else {
        Write-Output 'PASS native-smoke formats=7 animations=2 navigation=4 continuity=1 error-recovery=1 webdriver=absent'
    }
    $nativeSmokeCompleted = $true
}
finally {
    if ($null -ne $process) {
        try {
            $process.Refresh()
            if (-not $process.HasExited) {
                $closingHelpers = @(Get-DirectCodecHelpers -RootProcessId $process.Id)
                if ($closingHelpers.Count -gt 1) {
                    $ids = @($closingHelpers | ForEach-Object { $_.ProcessId })
                    $helperCleanupFailure = (
                        "ImgViewer had multiple codec helpers at shutdown: $($ids -join ', ')."
                    )
                }
                $helperProcessIds = @(
                    @($helperProcessIds) +
                        @($closingHelpers | ForEach-Object { [int]$_.ProcessId }) |
                        Sort-Object -Unique
                )
            }
        }
        catch {}
    }
    if ($window -ne [IntPtr]::Zero) {
        [ImgViewerNativeSmoke]::PostMessageW($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    }
    if ($null -ne $process) {
        if (-not $process.WaitForExit(3000)) {
            try {
                $process.Kill()
                [void]$process.WaitForExit(2000)
            }
            catch {}
        }
        $process.Dispose()
    }
    foreach ($observedHelperProcessId in @($helperProcessIds | Sort-Object -Unique)) {
        $helperExitDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while ((Test-CodecHelperProcessRunning -ProcessId $observedHelperProcessId) -and
            [DateTime]::UtcNow -lt $helperExitDeadline) {
            Start-Sleep -Milliseconds 50
        }
        if (Test-CodecHelperProcessRunning -ProcessId $observedHelperProcessId) {
            $helperCleanupFailure = (
                "Codec helper PID $observedHelperProcessId remained alive after ImgViewer exited."
            )
            try {
                Stop-Process -Id $observedHelperProcessId -Force -ErrorAction SilentlyContinue
            }
            catch {}
        }
    }
    if (-not $helperCleanupFailure -and
        $helperPersistenceVerified -and
        $nativeSmokeCompleted) {
        Write-Output (
            "PASS codec-helper-runtime sibling=verified direct-child=1 " +
            "persistent-pid=$helperProcessId orphan=absent webdriver=absent"
        )
    }
    $resolvedSmoke = [IO.Path]::GetFullPath($smokeDirectory)
    if ($resolvedSmoke.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($resolvedSmoke).StartsWith('ImgViewer-native-smoke-', [StringComparison]::Ordinal)) {
        if ([IO.Directory]::Exists($resolvedSmoke)) {
            [IO.Directory]::Delete($resolvedSmoke, $true)
        }
    }
    if ($helperCleanupFailure) {
        throw $helperCleanupFailure
    }
}
