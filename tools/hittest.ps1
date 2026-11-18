# Asks the window what Windows would do with a real click at client points
# (WM_NCHITTEST), so tests cover the same path as the physical mouse.
#   .\tools\hittest.ps1 "x,y,expected,label" ...
# expected: client | caption | maxbutton | left | right | top | bottom
param([string[]]$Cases)
if (-not ('HT' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class HT {
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  public struct POINT { public int X, Y; }
}
'@ }
$names = @{ 1 = 'client'; 2 = 'caption'; 9 = 'maxbutton'; 10 = 'left'; 11 = 'right'; 12 = 'top'; 15 = 'bottom' }
$h = (Get-Process veyra | Sort-Object StartTime | Select-Object -First 1).MainWindowHandle
$fail = 0
foreach ($case in $Cases) {
    $x, $y, $want, $label = $case.Split(',')
    $p = New-Object HT+POINT; $p.X = [int]$x; $p.Y = [int]$y
    [HT]::ClientToScreen($h, [ref]$p) | Out-Null
    $l = [IntPtr]((($p.Y -band 0xffff) -shl 16) -bor ($p.X -band 0xffff))
    $code = [int][HT]::SendMessage($h, 0x0084, [IntPtr]0, $l)
    $got = $names[$code]; if (-not $got) { $got = "code $code" }
    $ok = $got -eq $want
    if (-not $ok) { $fail++ }
    '{0,-4} {1,-38} want {2,-9} got {3}' -f $(if ($ok) { 'OK' } else { 'FAIL' }), $label, $want, $got
}
"failures: $fail"
