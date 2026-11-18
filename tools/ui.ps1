# UI test driver for the native sidebar (no real mouse movement).
#   .\tools\ui.ps1 click <x> <y>        click in sidebar client coords (physical px)
#   .\tools\ui.ps1 type "<text>"        type into the address field and press Enter
#   .\tools\ui.ps1 set "<text>"         set the address field text (shows suggestions if focused)
#   .\tools\ui.ps1 key <vk>             key press in the address field (38 up, 40 down, 27 esc)
param([string]$Cmd, [string]$A, [string]$B)
if (-not ('UiWin' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class UiWin {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
}
'@ }
$main = (Get-Process veyra | Select-Object -First 1).MainWindowHandle
$side = [UiWin]::FindWindowEx($main, [IntPtr]::Zero, 'BrowserSidebar', $null)
$edit = [UiWin]::FindWindowEx($side, [IntPtr]::Zero, 'Edit', $null)
switch ($Cmd) {
    'click' {
        $lp = [IntPtr](([int]$B -shl 16) -bor [int]$A)
        [UiWin]::SendMessage($side, 0x0200, [IntPtr]::Zero, $lp) | Out-Null  # WM_MOUSEMOVE
        [UiWin]::SendMessage($side, 0x0201, [IntPtr]1, $lp) | Out-Null       # WM_LBUTTONDOWN
        [UiWin]::SendMessage($side, 0x0202, [IntPtr]::Zero, $lp) | Out-Null  # WM_LBUTTONUP
    }
    'set' {
        [UiWin]::SendMessage($edit, 0x000C, [IntPtr]::Zero, $A) | Out-Null   # WM_SETTEXT (no Enter)
    }
    'key' {
        [UiWin]::SendMessage($edit, 0x0100, [IntPtr][int]$A, [IntPtr]::Zero) | Out-Null  # WM_KEYDOWN vk
    }
    'type' {
        [UiWin]::SendMessage($edit, 0x000C, [IntPtr]::Zero, $A) | Out-Null   # WM_SETTEXT
        [UiWin]::SendMessage($edit, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero) | Out-Null  # WM_KEYDOWN Enter
    }
}
