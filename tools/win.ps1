# Sends synthetic mouse/keyboard messages to the browser's main window
# (no real cursor movement). Coordinates are client pixels.
#   .\tools\win.ps1 move X Y | down X Y | up X Y | click X Y | rclick X Y | text "abc" | key VK
param([string]$Cmd, [string]$A, [string]$B)
if (-not ('WinMsg' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class WinMsg { [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@ }
$h = (Get-Process veyra | Select-Object -First 1).MainWindowHandle
function Get-LParam($x, $y) { [IntPtr]((([int]$y) -shl 16) -bor ([int]$x -band 0xffff)) }
switch ($Cmd) {
    'move'  { [WinMsg]::SendMessage($h, 0x0200, [IntPtr]0, (Get-LParam $A $B)) | Out-Null }
    'down'  { [WinMsg]::SendMessage($h, 0x0201, [IntPtr]1, (Get-LParam $A $B)) | Out-Null }
    'up'    { [WinMsg]::SendMessage($h, 0x0202, [IntPtr]0, (Get-LParam $A $B)) | Out-Null }
    'click' {
        [WinMsg]::SendMessage($h, 0x0200, [IntPtr]0, (Get-LParam $A $B)) | Out-Null
        [WinMsg]::SendMessage($h, 0x0201, [IntPtr]1, (Get-LParam $A $B)) | Out-Null
        [WinMsg]::SendMessage($h, 0x0202, [IntPtr]0, (Get-LParam $A $B)) | Out-Null
    }
    'rclick' {
        [WinMsg]::SendMessage($h, 0x0200, [IntPtr]0, (Get-LParam $A $B)) | Out-Null
        [WinMsg]::SendMessage($h, 0x0204, [IntPtr]2, (Get-LParam $A $B)) | Out-Null
        [WinMsg]::SendMessage($h, 0x0205, [IntPtr]0, (Get-LParam $A $B)) | Out-Null
    }
    'text'  { foreach ($ch in [char[]]$A) { [WinMsg]::SendMessage($h, 0x0102, [IntPtr][int]$ch, [IntPtr]0) | Out-Null } }
    'key'   { [WinMsg]::SendMessage($h, 0x0100, [IntPtr][int]$A, [IntPtr]0) | Out-Null }
    # Wheel down one notch at client (X,Y) (WM_MOUSEWHEEL takes screen coordinates).
    'wheel' {
        if (-not ('WinPt' -as [type])) { Add-Type 'using System; using System.Runtime.InteropServices; public class WinPt { [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p); public struct POINT { public int X, Y; } }' }
        $p = New-Object WinPt+POINT; $p.X = [int]$A; $p.Y = [int]$B; [WinPt]::ClientToScreen($h, [ref]$p) | Out-Null
        [WinMsg]::SendMessage($h, 0x020A, [IntPtr]((-120) -shl 16), (Get-LParam $p.X $p.Y)) | Out-Null
    }
}
