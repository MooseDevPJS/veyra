# Closes running test instances (they lock veyra.exe) and builds release.
# Only the copies built here: never the installed Veyra the user is browsing with.
$target = (Resolve-Path "$PSScriptRoot\..\target").Path
Get-Process veyra,browser -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($target, 'OrdinalIgnoreCase') } | Stop-Process -Force
Start-Sleep -Milliseconds 800
cargo build --release 2>&1 | Select-String -Pattern '^(error|warning)' -Context 0,8
"built: $((Get-Item "$PSScriptRoot\..\target\release\veyra.exe").LastWriteTime)"
