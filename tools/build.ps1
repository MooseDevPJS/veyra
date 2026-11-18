# Closes running test instances (they lock veyra.exe) and builds release.
Get-Process veyra,browser -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800
cargo build --release 2>&1 | Select-String -Pattern '^(error|warning)' -Context 0,8
"built: $((Get-Item "$PSScriptRoot\..\target\release\veyra.exe").LastWriteTime)"
