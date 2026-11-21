# Screenshot of the running browser window -> target\shots\<Name>.png
param([string]$Name = 'shot', [string]$Proc = 'veyra')
Add-Type -AssemblyName System.Drawing
if (-not ('ShotWin' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class ShotWin { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
public struct RECT { public int L,T,R,B; } }
'@ }
# The copy built here first (never the installed Veyra the user is using).
$p = Get-Process $Proc | Sort-Object { -not ($_.Path -like "*\target\*") }, StartTime | Select-Object -First 1
$h = $p.MainWindowHandle
[ShotWin]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 600
$r = New-Object ShotWin+RECT
[ShotWin]::GetWindowRect($h, [ref]$r) | Out-Null
$b = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
[Drawing.Graphics]::FromImage($b).CopyFromScreen($r.L, $r.T, 0, 0, $b.Size)
$dir = "$PSScriptRoot\..\target\shots"; New-Item -ItemType Directory -Force $dir | Out-Null
$out = "$dir\$Name.png"
$b.Save($out)
$out
