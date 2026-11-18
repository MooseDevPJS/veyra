# Moves the REAL cursor to client point (X,Y) of the browser window.
param([int]$X, [int]$Y)
if (-not ('CurWin' -as [type])) { Add-Type @"
using System; using System.Runtime.InteropServices;
public class CurWin { [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
public struct POINT { public int X, Y; } }
"@ }
$h = (Get-Process veyra | Sort-Object StartTime | Select-Object -First 1).MainWindowHandle
$p = New-Object CurWin+POINT; $p.X = $X; $p.Y = $Y
[CurWin]::ClientToScreen($h, [ref]$p) | Out-Null
[CurWin]::SetCursorPos($p.X, $p.Y) | Out-Null
