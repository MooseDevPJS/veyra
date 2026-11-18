# Captures the browser window N times, every Ms milliseconds, right after
# moving the real cursor to client (X,Y): frames of an animation.
#   .\tools\frames.ps1 -X 27 -Y 400 -Name open
param([int]$X, [int]$Y, [string]$Name = 'frame', [int]$N = 8, [int]$Ms = 35)
Add-Type -AssemblyName System.Drawing
if (-not ('FrWin' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class FrWin { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
[DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
public struct RECT { public int L,T,R,B; } public struct POINT { public int X, Y; } }
'@ }
$h = (Get-Process veyra | Sort-Object StartTime | Select-Object -First 1).MainWindowHandle
[FrWin]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [FrWin]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
[FrWin]::SetForegroundWindow($h) | Out-Null; Start-Sleep -Milliseconds 400
$r = New-Object FrWin+RECT; [FrWin]::GetWindowRect($h, [ref]$r) | Out-Null
$p = New-Object FrWin+POINT; $p.X = $X; $p.Y = $Y; [FrWin]::ClientToScreen($h, [ref]$p) | Out-Null
[FrWin]::SetCursorPos($p.X, $p.Y) | Out-Null
$w = 420; $hh = $r.B - $r.T
for ($i = 0; $i -lt $N; $i++) {
    $b = New-Object Drawing.Bitmap $w, $hh
    [Drawing.Graphics]::FromImage($b).CopyFromScreen($r.L, $r.T, 0, 0, $b.Size)
    $b.Save("A:\Cooding\DEsigns\Browser\claude\$Name-$i.png"); $b.Dispose()
    Start-Sleep -Milliseconds $Ms
}
