# Presses a real key chord in the browser, ONLY if it is the foreground
# window (otherwise the keys would go to another app).
#   .\tools\keys.ps1 0x11,0x54      Ctrl+T
#   .\tools\keys.ps1 0x7B           F12
param([int[]]$Keys)
if (-not ('KeysWin' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class KeysWin {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
'@ }
$h = (Get-Process veyra | Sort-Object StartTime | Select-Object -First 1).MainWindowHandle
for ($i = 0; $i -lt 3 -and [KeysWin]::GetForegroundWindow() -ne $h; $i++) {
    [KeysWin]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [KeysWin]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
    [KeysWin]::SetForegroundWindow($h) | Out-Null
    Start-Sleep -Milliseconds 300
}
if ([KeysWin]::GetForegroundWindow() -ne $h) { Write-Output 'NOT foreground: keys not sent'; exit 1 }
foreach ($k in $Keys) { [KeysWin]::keybd_event([byte]$k, 0, 0, [UIntPtr]::Zero) }
[array]::Reverse($Keys)
foreach ($k in $Keys) { [KeysWin]::keybd_event([byte]$k, 0, 2, [UIntPtr]::Zero) }
Start-Sleep -Milliseconds 300
