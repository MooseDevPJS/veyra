# RAM test (edits settings.json for -Suspend/-Unload/-LowEnd): starts a fresh browser with the given tabs, plays the YouTube one,
# waits, prints private memory per process type and the total.
#   .\tools\ram-test.ps1 -Urls a,b,c [-Cycle 6] [-LowEnd 1 -Suspend 20 -Unload 60] [-Wait 30]
param([string[]]$Urls, [int]$Cycle = 0, [int]$Wait = 30, [int]$Again = 0, [int]$Suspend = -1, [int]$Unload = -1, [int]$LowEnd = -1)
$settingsFile = "$env:LOCALAPPDATA\Veyra\settings.json"
if (Test-Path $settingsFile) {
    $s = Get-Content $settingsFile -Raw | ConvertFrom-Json
    if ($Suspend -ge 0) { $s.suspend_after_secs = $Suspend }
    if ($Unload -ge 0) { $s.unload_after_secs = $Unload }
    if ($LowEnd -ge 0) { $s | Add-Member -Force NoteProperty low_end_mode ([bool]$LowEnd) }
    [IO.File]::WriteAllText($settingsFile, ($s | ConvertTo-Json -Depth 5))
}
$root = Split-Path $PSScriptRoot
Get-Process veyra -ErrorAction SilentlyContinue | ForEach-Object { $_.CloseMainWindow() | Out-Null }
Start-Sleep 2
Remove-Item "$env:LOCALAPPDATA\Veyra\session.json" -ErrorAction SilentlyContinue
$env:BROWSER_DEVTOOLS_PORT = 9222; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $null
$args = @($Urls | ForEach-Object { "`"$_`"" })
if ($Cycle -gt 0) { $args += @('--cycle', $Cycle) }
$p = Start-Process "$root\target\release\veyra.exe" -ArgumentList $args -RedirectStandardError "$root\ram-test.log" -PassThru
Start-Sleep (12 + $Cycle * $Urls.Count)
if ($Urls -match 'youtube') {
    $cdp = "$root\tools\cdp.mjs"
    $xy = (node $cdp youtube "(() => { scrollTo(0,0); const p=document.querySelector('#movie_player'); if(!p) return ''; const r=p.getBoundingClientRect(); return Math.round(r.x+r.width/2)+' '+Math.round(r.y+r.height/2) })()") -replace '"', ''
    if ($xy) { $c = $xy -split ' '; node $cdp youtube click $c[0] $c[1] | Out-Null }
}
function Show-Ram {
$procs = Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'veyra.exe' -or ($_.Name -eq 'msedgewebview2.exe' -and $_.CommandLine -like '*\Browser\profile*') }
$rows = foreach ($q in $procs) {
    $type = if ($q.CommandLine -match '--type=([\w-]+)') { $Matches[1] } elseif ($q.Name -eq 'veyra.exe') { 'app' } else { 'webview-main' }
    [pscustomobject]@{ Type = $type; MB = [math]::Round((Get-Process -Id $q.ProcessId).PrivateMemorySize64 / 1MB) }
}
$rows | Group-Object Type | ForEach-Object { '{0,-14} {1,5} MB ({2})' -f $_.Name, ($_.Group | Measure-Object MB -Sum).Sum, $_.Count }
'TOTAL          {0,5} MB in {1} processes' -f ($rows | Measure-Object MB -Sum).Sum, $rows.Count
if ($Urls -match 'youtube') { node "$root\tools\cdp.mjs" youtube "(() => { const v=document.querySelector('video'); return v ? (!v.paused?'playing':'paused')+' t='+Math.round(v.currentTime)+' ad='+document.querySelector('#movie_player').classList.contains('ad-showing') : 'no video (tab unloaded?)' })()" }
}
Start-Sleep $Wait; '--- after play'; Show-Ram
if ($Again -gt 0) { Start-Sleep $Again; "--- $Again s later"; Measure }
