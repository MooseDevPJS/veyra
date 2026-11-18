# End-to-end test of the browser UI. Start it first with
#   $env:BROWSER_DEVTOOLS_PORT=9222; Start-Process target\release\veyra.exe
# Uses window messages for the mouse, real keyboard input (SendInput) for
# shortcuts, and checks results in session.json / settings.json / clipboard.
$ErrorActionPreference = 'Continue'
$tools = $PSScriptRoot
$data = "$env:LOCALAPPDATA\Veyra"
$w = "$tools\win.ps1"
$cdp = "$tools\cdp.mjs"
if (-not ('FT' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class FT {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public static void Key(byte vk, bool up) { keybd_event(vk, 0, up ? 2u : 0u, UIntPtr.Zero); }
}
'@ }
$script:fails = 0
function Check($ok, $what) {
    if ($ok) { "OK    $what" } else { "FAIL  $what"; $script:fails++ }
}
function Main { Get-Process veyra | Sort-Object StartTime | Select-Object -First 1 }
function Title { (Main).MainWindowTitle }
# Brings the browser to the front. A lone Alt tap lifts Windows'
# foreground lock. Returns $false (and real keys must NOT be sent) when the
# browser still is not the foreground window.
function Front {
    $h = (Main).MainWindowHandle
    for ($i = 0; $i -lt 3; $i++) {
        if ([FT]::GetForegroundWindow() -eq $h) { return $true }
        [FT]::Key(0x12, $false); [FT]::Key(0x12, $true)
        [FT]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 300
    }
    return ([FT]::GetForegroundWindow() -eq $h)
}
# Real key chord, e.g. Chord 0x11,0x54 (Ctrl+T).
function Chord([byte[]]$keys) {
    if (-not (Front)) { throw 'browser is not the foreground window: real keys not sent' }
    foreach ($k in $keys) { [FT]::Key($k, $false) }
    foreach ($k in ($keys[($keys.Length - 1)..0])) { [FT]::Key($k, $true) }
    Start-Sleep -Milliseconds 500
}
function Session { Start-Sleep 6; Get-Content "$data\session.json" -Raw | ConvertFrom-Json }
function Unpinned($s) { @($s.tabs | Where-Object { -not $_.pinned }) }
# Pinned tabs are shared by all windows, in pins.json (written at once).
function Pins { Start-Sleep -Milliseconds 500; (Get-Content "$data\pins.json" -Raw | ConvertFrom-Json).Count }

"== 0. Clean start (4 known tabs)"
Get-Process veyra -ErrorAction SilentlyContinue | ForEach-Object { $_.CloseMainWindow() | Out-Null }
Start-Sleep 3
Get-Process veyra -ErrorAction SilentlyContinue | Stop-Process -Force
$clean = @{ active = 0; tabs = @(
    @{ url = 'https://example.com/'; title = 'Example Domain'; pinned = $false },
    @{ url = 'https://www.wikipedia.org/'; title = 'Wikipedia'; pinned = $false },
    @{ url = 'https://github.com/'; title = 'GitHub'; pinned = $false },
    @{ url = 'https://noai.duckduckgo.com/'; title = 'DuckDuckGo'; pinned = $false }
) } | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText("$data\session.json", $clean)
[IO.File]::WriteAllText("$data\bookmarks.json", "[]")   # no bookmark row: fixed coordinates
[IO.File]::WriteAllText("$data\pins.json", "[]")        # no pinned tiles either
$env:BROWSER_DEVTOOLS_PORT = 9222
Start-Process (Join-Path (Split-Path $tools) 'target\release\veyra.exe') -RedirectStandardError (Join-Path (Split-Path $tools) 'perf.log')
Start-Sleep 8

"== 1. Command bar from the address pill"
& $w click 130 63; Start-Sleep -Milliseconds 200
& $w text 'example.org'; & $w key 13; Start-Sleep 3
Check ((Title) -like '*Example*') "address pill opens example.org (title: $(Title))"

"== 2. Shortcuts (real keyboard)"
$before = (Unpinned (Session)).Count
Chord 0x11, 0x54                       # Ctrl+T -> floating bar
Start-Sleep -Milliseconds 300; & $w text 'example.net'; Start-Sleep -Milliseconds 300
& $w key 13; Start-Sleep 3
$after = (Unpinned (Session)).Count
Check ($after -eq $before + 1) "Ctrl+T + Enter opens a new tab ($before -> $after)"
Chord 0x11, 0x57                       # Ctrl+W
$closed = (Unpinned (Session)).Count
Check ($closed -eq $after - 1) "Ctrl+W closes it ($after -> $closed)"
Chord 0x11, 0x10, 0x54                 # Ctrl+Shift+T
$reopened = (Unpinned (Session)).Count
Check ($reopened -eq $closed + 1) "Ctrl+Shift+T reopens it ($closed -> $reopened)"
Chord 0x11, 0x10, 0x43; $u1 = Get-Clipboard
Chord 0x11, 0x09                                 # Ctrl+Tab
Chord 0x11, 0x10, 0x43; $u2 = Get-Clipboard
Check ($u1 -ne $u2) "Ctrl+Tab switches tab ($u1 -> $u2)"
Chord 0x11, 0x10, 0x43                 # Ctrl+Shift+C
Check ((Get-Clipboard) -like 'http*') "Ctrl+Shift+C copies the address ($(Get-Clipboard))"

"== 3. Tab clicks"
& $w click 100 145; Start-Sleep 2; $a = Title
& $w click 100 181; Start-Sleep 2; $b = Title
Check ($a -ne $b) "clicking tab rows switches ('$a' / '$b')"

"== 4. Tab menu: duplicate, pin, unpin"
$n0 = (Session).tabs.Count
& $w rclick 100 145; Start-Sleep -Milliseconds 300
& $w move 150 195; & $w down 150 195; & $w up 150 195   # 2nd item: Duplicate
$n1 = (Session).tabs.Count
Check ($n1 -eq $n0 + 1) "Duplicate adds a tab ($n0 -> $n1)"
& $w rclick 100 145; Start-Sleep -Milliseconds 300
& $w move 150 163; & $w down 150 163; & $w up 150 163   # 1st item: Pin
$p1 = Pins
Check ($p1 -ge 1) "Pin adds a tile (pinned: $p1)"
& $w rclick 47 113; Start-Sleep -Milliseconds 300
& $w move 90 131; & $w down 90 131; & $w up 90 131      # 1st item: Unpin
Check ((Pins) -eq $p1 - 1) "Unpin removes the tile"

"== 5. Drag to reorder"
$s = Session; $first = (Unpinned $s)[0].url
& $w move 100 109; & $w down 100 109
foreach ($y in 130, 170, 210) { & $w move 100 $y; Start-Sleep -Milliseconds 30 }
& $w up 100 210
$s = Session
Check ((Unpinned $s)[0].url -ne $first) "dragging the first tab down moves it"

"== 6. Page context menu on a link + Copy link"
& $w click 130 63; Start-Sleep -Milliseconds 200; & $w text 'example.com'; & $w key 13; Start-Sleep 3
$xy = (node $cdp example.com "(() => { const r=document.querySelector('a').getBoundingClientRect(); return Math.round(r.x+r.width/2)+','+Math.round(r.y+r.height/2) })()") -replace '"', ''
$p = $xy.Split(','); $x = [int]$p[0] + 260; $y = [int]$p[1] + 8
Set-Clipboard -Value 'none'
& $w rclick $x $y; Start-Sleep -Milliseconds 400
$top = $y - (6 + 3 * 32 + 9 + 32 + 6)        # menu opens upward near the bottom
& $w move ($x + 40) ($top + 6 + 2 * 32 + 16); & $w down ($x + 40) ($top + 6 + 2 * 32 + 16); & $w up ($x + 40) ($top + 6 + 2 * 32 + 16)
Start-Sleep -Milliseconds 300
Check ((Get-Clipboard) -like '*iana.org*') "Copy link puts the link on the clipboard ($(Get-Clipboard))"

"== 7. Hit testing (real-mouse path)"
& "$tools\hittest.ps1" -Cases @("23,24,client,back", "207,24,maxbutton,maximize", "237,24,client,close", "257,300,client,sidebar edge", "150,650,caption,empty sidebar", "700,400,client,page", "700,3,top,top border")

"== 8. Settings round trip"
& $w click 23 775; Start-Sleep 3
node $cdp browser.settings "(() => { [...document.querySelectorAll('.seg[data-key=glass] button')][1].click(); return 1 })()" | Out-Null
Start-Sleep 1
Check (-not (Get-Content "$data\settings.json" -Raw | ConvertFrom-Json).glass) "Solid material is saved"
node $cdp browser.settings "(() => { [...document.querySelectorAll('.seg[data-key=glass] button')][0].click(); return 1 })()" | Out-Null
Start-Sleep 1
Check ((Get-Content "$data\settings.json" -Raw | ConvertFrom-Json).glass) "Glass material is saved"

"failures: $script:fails"
