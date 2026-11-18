# Sums memory of the spike and its WebView2 processes (matched by profile path).
# Private = memory only this browser uses (closest to Task Manager's figure).
$procs = Get-CimInstance Win32_Process | Where-Object {
    $_.Name -eq 'engine-spike.exe' -or
    ($_.Name -eq 'msedgewebview2.exe' -and $_.CommandLine -like '*BrowserSpike*')
}
$rows = foreach ($p in $procs) {
    $gp = Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue
    if (-not $gp) { continue }
    $type = if ($p.CommandLine -match '--type=([\w-]+)') { $Matches[1] } else { 'main' }
    [pscustomobject]@{ Type = $type; PrivateMB = [math]::Round($gp.PrivateMemorySize64 / 1MB); CpuSec = [math]::Round($gp.CPU, 1) }
}
$rows | Sort-Object PrivateMB -Descending | Format-Table -AutoSize
"TOTAL private: {0} MB in {1} processes" -f ($rows | Measure-Object PrivateMB -Sum).Sum, $rows.Count
