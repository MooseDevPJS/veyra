# Links opened with the middle button or Ctrl+click go to a background tab
# (the current tab stays in front); a plain click on a target=_blank link
# still switches to the new tab. Clean start with 1 tab; window messages
# (no real mouse), checks in session.json.
$ErrorActionPreference = 'Continue'
$tools = $PSScriptRoot
$data = "$env:LOCALAPPDATA\Veyra"
$cdp = "$tools\cdp.mjs"
if (-not ('BT' -as [type])) { Add-Type @'
using System; using System.Runtime.InteropServices;
public class BT { [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l); }
'@ }
$script:fails = 0
function Check($ok, $what) { if ($ok) { "OK    $what" } else { "FAIL  $what"; $script:fails++ } }
function Main { Get-Process veyra | Sort-Object StartTime | Select-Object -First 1 }
function Title { (Get-Process -Id (Main).Id).MainWindowTitle }
function Session { Start-Sleep 6; Get-Content "$data\session.json" -Raw | ConvertFrom-Json }
function Tabs { @((Session).tabs) }
function ActiveUrl { $s = Session; @($s.tabs)[$s.active].url }
function L($x, $y) { [IntPtr]((([int]$y) -shl 16) -bor ([int]$x -band 0xffff)) }
function Msg($m, $w) { [BT]::SendMessage((Main).MainWindowHandle, $m, [IntPtr]$w, (L 800 450)) | Out-Null }
# The whole page becomes one link (target=_blank, so a plain click opens a tab too).
function LinkPage($n) {
    node $cdp example.com "document.body.innerHTML='<a href=""https://example.org/?$n"" target=""_blank"" style=""position:fixed;inset:0;display:block;font-size:40px"">link $n</a>'" | Out-Null
    Start-Sleep -Milliseconds 300
}

"== Clean start (1 tab)"
Get-Process veyra -ErrorAction SilentlyContinue | ForEach-Object { $_.CloseMainWindow() | Out-Null }
Start-Sleep 3
Get-Process veyra -ErrorAction SilentlyContinue | Stop-Process -Force
$clean = @{ active = 0; tabs = @(@{ url = 'https://example.com/'; title = 'Example Domain'; pinned = $false }) } | ConvertTo-Json -Depth 4
[IO.File]::WriteAllText("$data\session.json", $clean)
[IO.File]::WriteAllText("$data\pins.json", "[]")
$env:BROWSER_DEVTOOLS_PORT = 9222
Start-Process (Join-Path (Split-Path $tools) 'target\release\veyra.exe')
Start-Sleep 8

"== 1. Middle click"
LinkPage 1
Msg 0x0200 0; Msg 0x0207 0x10; Msg 0x0208 0; Start-Sleep 2
$tabs = Tabs
Check ($tabs.Count -eq 2) "a new tab opened ($($tabs.Count) tabs)"
$u = ActiveUrl
Check ($u -eq 'https://example.com/') "the current tab stays in front ($u)"
"== 2. Second middle click: lines up after the first"
LinkPage 2
Msg 0x0200 0; Msg 0x0207 0x10; Msg 0x0208 0; Start-Sleep 2
$tabs = Tabs
Check ($tabs.Count -eq 3 -and $tabs[1].url -like '*?1' -and $tabs[2].url -like '*?2') "order: $(($tabs | ForEach-Object { $_.url }) -join ' , ')"
$u = ActiveUrl
Check ($u -eq 'https://example.com/') "still on the first tab ($u)"
"== 3. Ctrl+click"
LinkPage 3
Msg 0x0200 0; Msg 0x0201 0x9; Msg 0x0202 0x8; Start-Sleep 2
$tabs = Tabs
Check ($tabs.Count -eq 4) "Ctrl+click opened a tab ($($tabs.Count) tabs)"
$u = ActiveUrl
Check ($u -eq 'https://example.com/') "the current tab stays in front ($u)"
"== 4. Plain click on a target=_blank link"
LinkPage 4
Msg 0x0200 0; Msg 0x0201 0x1; Msg 0x0202 0; Start-Sleep 3
$tabs = Tabs
Check ($tabs.Count -eq 5) "a tab opened ($($tabs.Count) tabs)"
$u = ActiveUrl
Check ($u -like '*?4') "and it comes to the front ($u)"
"fails: $script:fails"
