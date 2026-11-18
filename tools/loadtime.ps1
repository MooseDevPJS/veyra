# Measures page load (navigationStart -> loadEventEnd) of a URL N times in
# the running browser (needs BROWSER_DEVTOOLS_PORT=9222).
param([string]$Url = 'https://www.repubblica.it', [int]$Times = 3)
$cdp = Join-Path $PSScriptRoot 'cdp.mjs'
$w = Join-Path $PSScriptRoot 'win.ps1'
$host_ = ([uri]$Url).Host
for ($i = 1; $i -le $Times; $i++) {
    & $w click 130 63; Start-Sleep -Milliseconds 200
    & $w text $Url; & $w key 13
    Start-Sleep 9
    node $cdp $host_ "(() => { const t = performance.getEntriesByType('navigation')[0]; return Math.round(t.domContentLoadedEventEnd) + ' ms DOM, ' + Math.round(t.loadEventEnd) + ' ms load, ' + performance.getEntriesByType('resource').length + ' resources' })()"
}
