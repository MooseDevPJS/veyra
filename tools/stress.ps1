# Stress: many quick UI actions in a row, as a real user would do them.
# Run the browser with BROWSER_PERF=1 and stderr redirected to perf.log.
param([int]$Rounds = 6)
$w = Join-Path $PSScriptRoot 'win.ps1'
for ($r = 1; $r -le $Rounds; $r++) {
    # New tab from the sidebar row, type, Enter.
    & $w click 100 417; Start-Sleep -Milliseconds 150
    & $w text "test $r"; & $w key 13; Start-Sleep -Milliseconds 700
    # Switch between the first tabs.
    foreach ($y in 165, 201, 237, 273) { & $w click 100 $y; Start-Sleep -Milliseconds 250 }
    # Settings and back.
    & $w click 23 775; Start-Sleep -Milliseconds 600
    # Right-click menu open + close with Esc.
    & $w rclick 100 201; Start-Sleep -Milliseconds 200; & $w key 27; Start-Sleep -Milliseconds 200
    # Address bar on the current tab.
    & $w click 130 63; Start-Sleep -Milliseconds 150; & $w text "example.com"; & $w key 13; Start-Sleep -Milliseconds 800
}
