# Drives the DirFleet.exe that start.ps1 started, for manual checks, without moving the pointer
# or typing for the user: buttons and menus through UI Automation, keys as messages posted to the
# app's own window. Pointer input (clicking rows, dragging) cannot be driven this way.
#
# Only that instance is ever touched (its process id is in target\drive\pid): a posted key lands
# in whatever a window has focused, so another DirFleet, which a person may be using, is left alone.
#
#   windows\tools\start.ps1 -Folder C:\some\scratch\folder
#   . windows\tools\drive.ps1
#   Get-DirFleetWindows; Invoke-Named 'More'; Invoke-MenuItem 'New Window'; Send-Key 0x28 -Times 3
#   windows\tools\capture.ps1 -ProcessId (Get-Mine).Id -Out shot.png
$driveDir = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path 'target\drive'
function Get-Mine {
    $id = Get-Content (Join-Path $driveDir 'pid') -ErrorAction SilentlyContinue
    if ($id) { Get-Process -Id ([int]$id) -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -eq 'DirFleet' } }
}
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    public static List<IntPtr> TopLevel(uint pid) {
        var found = new List<IntPtr>();
        EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h)) found.Add(h); return true; }, IntPtr.Zero);
        return found;
    }
    public static string Title(IntPtr h) { var s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
    public static string Class(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
}
'@

function Get-DirFleetWindows {
    $process = Get-Mine
    if (-not $process) { return @() }
    [Win]::TopLevel([uint32]$process.Id) | ForEach-Object {
        $rect = New-Object Win+RECT
        [Win]::GetWindowRect($_, [ref]$rect) | Out-Null
        [pscustomobject]@{ Handle = $_; Title = [Win]::Title($_); Class = [Win]::Class($_); X = $rect.L; Y = $rect.T; W = $rect.R - $rect.L; H = $rect.B - $rect.T }
    } | Where-Object { $_.Class -eq 'WinUIDesktopWin32WindowClass' }
}

function Get-Root([IntPtr]$Handle) {
    if ($Handle -eq [IntPtr]::Zero) { $Handle = (Get-DirFleetWindows | Select-Object -First 1).Handle }
    [System.Windows.Automation.AutomationElement]::FromHandle($Handle)
}

function Find-Named([string]$Name, [IntPtr]$Handle = [IntPtr]::Zero, $Scope = $null) {
    $root = if ($Scope) { $Scope } else { Get-Root $Handle }
    $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}

function Invoke-Named([string]$Name, [IntPtr]$Handle = [IntPtr]::Zero) {
    $found = Find-Named $Name $Handle
    if (-not $found) { "no element named '$Name'"; return }
    $pattern = $null
    if ($found.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke(); "invoked '$Name'" }
    elseif ($found.TryGetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$pattern)) { $pattern.Expand(); "expanded '$Name'" }
    else { "'$Name' cannot be invoked" }
}

# Menus open in their own popup window, so the item is searched from the desktop root within this process.
function Invoke-MenuItem([string]$Name) {
    $process = Get-Mine
    $conditions = New-Object System.Windows.Automation.AndCondition(
        (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $process.Id)),
        (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)))
    $items = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, $conditions)
    foreach ($item in $items) {
        if ($item.Current.Name -like "$Name*") {
            $item.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            return "invoked menu item '$($item.Current.Name)'"
        }
    }
    "no menu item '$Name' among: $(($items | ForEach-Object { $_.Current.Name }) -join ' | ')"
}

function Get-Names([IntPtr]$Handle = [IntPtr]::Zero, [int]$Max = 80) {
    $root = Get-Root $Handle
    $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $all | Select-Object -First $Max | ForEach-Object { "$($_.Current.ControlType.ProgrammaticName.Replace('ControlType.', '')): '$($_.Current.Name)'" }
}

function Close-Window([IntPtr]$Handle) { [Win]::PostMessage($Handle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
function Move-Window([IntPtr]$Handle, [int]$X, [int]$Y, [int]$W, [int]$H) { [Win]::SetWindowPos($Handle, [IntPtr]::Zero, $X, $Y, $W, $H, 0x0014) | Out-Null }
function Trace-Since([int]$Before, [string]$Match = '.') {
    Get-Content (Join-Path $driveDir 'stderr.txt') | Select-Object -Skip $Before | Where-Object { $_ -match $Match }
}
function Trace-Count { (Get-Content (Join-Path $driveDir 'stderr.txt') -ErrorAction SilentlyContinue).Count }

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Keys {
    [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO {
        public int cbSize; public int flags; public IntPtr hwndActive, hwndFocus, hwndCapture, hwndMenuOwner, hwndMoveSize, hwndCaret;
        public int l, t, r, b;
    }
    [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint thread, ref GUITHREADINFO info);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint MapVirtualKey(uint code, uint type);
    public static IntPtr Focus(IntPtr top) {
        uint pid; uint thread = GetWindowThreadProcessId(top, out pid);
        var info = new GUITHREADINFO(); info.cbSize = Marshal.SizeOf(info);
        GetGUIThreadInfo(thread, ref info);
        return info.hwndFocus;
    }
    public static void Press(IntPtr target, uint vk) {
        uint scan = MapVirtualKey(vk, 0);
        PostMessage(target, 0x0100, (IntPtr)vk, (IntPtr)(1 | (scan << 16)));
        PostMessage(target, 0x0101, (IntPtr)vk, (IntPtr)(1 | (scan << 16) | 0xC0000000));
    }
}
'@
# Posts a key press to whatever has the keyboard inside the DirFleet window (messages to the app only; nothing is injected system-wide).
function Send-Key([uint32]$Vk, [IntPtr]$Handle = [IntPtr]::Zero, [int]$Times = 1) {
    if ($Handle -eq [IntPtr]::Zero) { $Handle = (Get-DirFleetWindows | Select-Object -First 1).Handle }
    $focus = [Keys]::Focus($Handle)
    if ($focus -eq [IntPtr]::Zero) { return "no focused window in the app (it is not the active window)" }
    for ($i = 0; $i -lt $Times; $i++) { [Keys]::Press($focus, $Vk); Start-Sleep -Milliseconds 40 }
    "sent key 0x$($Vk.ToString('X2')) x$Times to $focus"
}
