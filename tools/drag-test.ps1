# Several windows with the REAL mouse (a torn-off window follows the
# physical button). Clean start: 2 pinned tabs (pins.json) + 4 tabs. Then:
#  1. tear the 4th tab off: a window the size of A appears at once, has the
#     same pinned tabs, follows the pointer; released over A's page it stays;
#  2. drag that window's only tab: the window moves at once; over A's
#     sidebar it hides and A shows the tab, away again it comes back,
#     released on A's sidebar it goes into A;
#  3. a pinned tile dragged onto the page does not come off;
#  4. pinning a tab in one window pins it in the other;
#  5. closing A then B: B's tabs are the saved session; A's tabs can be
#     reopened in B with Ctrl+Shift+T.
# Moves the real cursor: do not touch the mouse while it runs.
$ErrorActionPreference = 'Continue'
$tools = $PSScriptRoot
$data = "$env:LOCALAPPDATA\Veyra"
if (-not ('TT' -as [type])) { Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public class TT {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public struct RECT { public int L, T, R, B; }
  public static List<IntPtr> Wins() {
    var l = new List<IntPtr>(); IntPtr h = IntPtr.Zero;
    while ((h = FindWindowEx(IntPtr.Zero, h, "VeyraMain", null)) != IntPtr.Zero) if (IsWindowVisible(h)) l.Add(h);
    return l;
  }
  public static List<IntPtr> All() {
    var l = new List<IntPtr>(); IntPtr h = IntPtr.Zero;
    while ((h = FindWindowEx(IntPtr.Zero, h, "VeyraMain", null)) != IntPtr.Zero) l.Add(h);
    return l;
  }
  public static RECT Rect(IntPtr h) { RECT r; GetWindowRect(h, out r); return r; }
  public static void Down() { mouse_event(2, 0, 0, 0, UIntPtr.Zero); }
  public static void Up() { mouse_event(4, 0, 0, 0, UIntPtr.Zero); }
  public static void Key(byte vk, bool up) { keybd_event(vk, 0, up ? 2u : 0u, UIntPtr.Zero); }
  // WM_APP + 10: normal tabs | pinned << 16.
  public static int Normal(IntPtr h) { return (int)SendMessage(h, 0x8000 + 10, IntPtr.Zero, IntPtr.Zero) & 0xffff; }
  public static int Pinned(IntPtr h) { return ((int)SendMessage(h, 0x8000 + 10, IntPtr.Zero, IntPtr.Zero) >> 16) & 0xffff; }
}
'@ }
$script:fails = 0
function Check($ok, $what) { if ($ok) { "OK    $what" } else { "FAIL  $what"; $script:fails++ } }
function Glide($fromX, $fromY, $toX, $toY, $steps = 15) {
    for ($i = 1; $i -le $steps; $i++) {
        [TT]::SetCursorPos([int]($fromX + ($toX - $fromX) * $i / $steps), [int]($fromY + ($toY - $fromY) * $i / $steps)) | Out-Null
        Start-Sleep -Milliseconds 12
    }
}
function Front($h) {
    for ($i = 0; $i -lt 3; $i++) {
        if ([TT]::GetForegroundWindow() -eq $h) { return $true }
        [TT]::Key(0x12, $false); [TT]::Key(0x12, $true)
        [TT]::SetForegroundWindow($h) | Out-Null; Start-Sleep -Milliseconds 300
    }
    return ([TT]::GetForegroundWindow() -eq $h)
}
function L($x, $y) { [IntPtr]((([int]$y) -shl 16) -bor ([int]$x -band 0xffff)) }
function Msg($h, $m, $w, $x, $y) { [TT]::SendMessage($h, $m, [IntPtr]$w, (L $x $y)) | Out-Null }

"== Clean start (2 pinned + 4 tabs)"
Get-Process veyra -ErrorAction SilentlyContinue | ForEach-Object { $_.CloseMainWindow() | Out-Null }
Start-Sleep 3
Get-Process veyra -ErrorAction SilentlyContinue | Stop-Process -Force
$pins = '[{"id":11,"url":"https://example.org/","title":"Example pinned"},{"id":12,"url":"https://www.iana.org/","title":"IANA"}]'
$clean = @{ active = 0; tabs = @(
    @{ url = 'https://example.com/'; title = 'Example Domain' },
    @{ url = 'https://www.wikipedia.org/'; title = 'Wikipedia' },
    @{ url = 'https://github.com/'; title = 'GitHub' },
    @{ url = 'https://noai.duckduckgo.com/'; title = 'DuckDuckGo' }
) } | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText("$data\pins.json", $pins)
[IO.File]::WriteAllText("$data\session.json", $clean)
[IO.File]::WriteAllText("$data\bookmarks.json", "[]")
if (Test-Path "$data\window.json") { [IO.File]::Delete("$data\window.json") }   # default size and place
Start-Process (Join-Path (Split-Path $tools) 'target\release\veyra.exe')
Start-Sleep 8
$a = [TT]::Wins()[0]
Front $a | Out-Null
$ra = [TT]::Rect($a)
"A at $($ra.L),$($ra.T) size $($ra.R - $ra.L)x$($ra.B - $ra.T)"
Check ([TT]::Normal($a) -eq 4 -and [TT]::Pinned($a) -eq 2) "A: 4 tabs + 2 pinned"
# Rows (bookmarks off, one row of tiles): tiles y 92-134, tabs from 148, 36 apart.
$row = { param($i) 148 + 17 + 36 * ($i - 1) }

"== 1. Tear the 4th tab off"
$sx = $ra.L + 100; $sy = $ra.T + (& $row 4)
[TT]::SetCursorPos($sx, $sy) | Out-Null; Start-Sleep -Milliseconds 300
[TT]::Down(); Start-Sleep -Milliseconds 80
Glide $sx $sy ($sx + 150) ($sy + 10) 8
Glide ($sx + 150) ($sy + 10) ($sx + 400) ($sy + 40) 6
$sw = [Diagnostics.Stopwatch]::StartNew()
while ([TT]::Wins().Count -lt 2 -and $sw.ElapsedMilliseconds -lt 5000) { Start-Sleep -Milliseconds 5 }
Check ([TT]::Wins().Count -eq 2) "a new window appears while the button is held ($($sw.ElapsedMilliseconds) ms)"
$b = [TT]::Wins() | Where-Object { $_ -ne $a } | Select-Object -First 1
Start-Sleep -Milliseconds 300
$rb1 = [TT]::Rect($b)
Check ((($rb1.R - $rb1.L) -eq ($ra.R - $ra.L)) -and (($rb1.B - $rb1.T) -eq ($ra.B - $ra.T))) "same size as A"
Glide ($sx + 400) ($sy + 40) ($sx + 600) ($sy + 140) 15
Start-Sleep -Milliseconds 100
$rb2 = [TT]::Rect($b)
Check ([Math]::Abs($rb2.L - $rb1.L - 200) -le 3 -and [Math]::Abs($rb2.T - $rb1.T - 100) -le 3) "it follows the pointer"
[TT]::Up(); Start-Sleep 2
Check ([TT]::Wins().Count -eq 2) "released over A's page: stays a separate window"
Check ([TT]::Normal($a) -eq 3) "A has 3 tabs left"
Check ([TT]::Normal($b) -eq 1 -and [TT]::Pinned($b) -eq 2) "B: its tab + the same 2 pinned"

"== 2. Drag B's only tab"
Front $b | Out-Null
$rb = [TT]::Rect($b)
$bx = $rb.L + 100; $by = $rb.T + (& $row 1)
[TT]::SetCursorPos($bx, $by) | Out-Null; Start-Sleep -Milliseconds 300
[TT]::Down(); Start-Sleep -Milliseconds 80
Glide $bx $by ($bx + 12) ($by + 2) 3          # a small move, still in the sidebar
Start-Sleep -Milliseconds 60
$rbm = [TT]::Rect($b)
Check ($rbm.L -ne $rb.L) "the window moves at once (moved $($rbm.L - $rb.L) px)"
$tx = $ra.L + 120; $ty = $ra.T + (& $row 2)
Glide ($bx + 12) ($by + 2) $tx $ty 25
Start-Sleep -Milliseconds 150
Check ([TT]::Wins().Count -eq 1) "over A's sidebar: B hides"
Check ([TT]::Normal($a) -eq 4) "and A shows the tab in its list ($([TT]::Normal($a)))"
Glide $tx $ty ($ra.L + 700) ($ra.T + 400) 15
Start-Sleep -Milliseconds 150
Check ([TT]::Wins().Count -eq 2) "away from the sidebar: B is back"
Check ([TT]::Normal($a) -eq 3) "and A's preview row is gone ($([TT]::Normal($a)))"
Glide ($ra.L + 700) ($ra.T + 400) $tx $ty 15
Start-Sleep -Milliseconds 150
[TT]::Up(); Start-Sleep 2
Check ([TT]::All().Count -eq 1) "released on A's sidebar: B closes"
Check ([TT]::Normal($a) -eq 4) "A has 4 tabs again"

"== 3. A pinned tile does not come off"
Front $a | Out-Null
$px = $ra.L + 48; $py = $ra.T + 113
[TT]::SetCursorPos($px, $py) | Out-Null; Start-Sleep -Milliseconds 300
[TT]::Down(); Start-Sleep -Milliseconds 80
Glide $px $py ($px + 600) ($py + 200) 15
Start-Sleep -Milliseconds 800
[TT]::Up(); Start-Sleep 1
Check ([TT]::All().Count -eq 1 -and [TT]::Pinned($a) -eq 2) "no new window, still 2 pinned"

"== 4. Pinned tabs are the same in every window"
# Tear the 1st tab off again to get B, then pin a tab in A.
$sx = $ra.L + 100; $sy = $ra.T + (& $row 1)
[TT]::SetCursorPos($sx, $sy) | Out-Null; Start-Sleep -Milliseconds 300
[TT]::Down(); Start-Sleep -Milliseconds 80
Glide $sx $sy ($sx + 500) ($sy + 150) 15
Start-Sleep -Milliseconds 1500
[TT]::Up(); Start-Sleep 3
$b = [TT]::Wins() | Where-Object { $_ -ne $a } | Select-Object -First 1
Msg $a 0x0200 0 100 (& $row 1); Msg $a 0x0204 2 100 (& $row 1); Msg $a 0x0205 0 100 (& $row 1)
Start-Sleep -Milliseconds 300
$my = (& $row 1) + 18
Msg $a 0x0200 0 150 $my; Msg $a 0x0201 1 150 $my; Msg $a 0x0202 0 150 $my   # 1st item: Pin
Start-Sleep 1
Check ([TT]::Pinned($a) -eq 3) "A: 3 pinned"
Check ([TT]::Pinned($b) -eq 3) "B: 3 pinned too"
$pinsNow = Get-Content "$data\pins.json" -Raw | ConvertFrom-Json
Check ($pinsNow.Count -eq 3) "pins.json has 3"

"== 5. Close A, then B"
$nb = [TT]::Normal($b)
[TT]::PostMessage($a, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null; Start-Sleep 3
Check ([TT]::All().Count -eq 1) "A closed"
if (Front $b) {
    [TT]::Key(0x11, $false); [TT]::Key(0x10, $false); [TT]::Key(0x54, $false)
    [TT]::Key(0x54, $true); [TT]::Key(0x10, $true); [TT]::Key(0x11, $true)
    Start-Sleep 2
    Check ([TT]::Normal($b) -eq $nb + 1) "Ctrl+Shift+T in B reopens a tab closed with A ($nb -> $([TT]::Normal($b)))"
} else { Check $false "B to the front for Ctrl+Shift+T" }
$nb = [TT]::Normal($b)
[TT]::PostMessage($b, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null; Start-Sleep 3
$s = Get-Content "$data\session.json" -Raw | ConvertFrom-Json
Check (@($s.tabs).Count -eq $nb) "the saved session is B's ($(@($s.tabs).Count) tabs, B had $nb)"
"fails: $script:fails"
