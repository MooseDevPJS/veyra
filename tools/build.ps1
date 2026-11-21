# Closes running test instances (they lock veyra.exe) and builds release.
# Only the copies built here: never the installed Veyra the user is browsing with.
$target = (Resolve-Path "$PSScriptRoot\..\target").Path
Get-Process veyra,browser -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($target, 'OrdinalIgnoreCase') } | Stop-Process -Force
Start-Sleep -Milliseconds 800
# No personal paths in the program: Rust keeps the source paths of the
# toolchain and libraries (the user folder's .cargo and .rustup) in its
# error messages, and the linker the path of the debug file. They become
# neutral names, read from Windows (no user name in this file either).
$repo = (Resolve-Path "$PSScriptRoot\..").Path
$env:RUSTFLAGS = "--remap-path-prefix=$($env:USERPROFILE)=~ --remap-path-prefix=$repo=veyra -C link-arg=/PDBALTPATH:veyra.pdb"
cargo build --release 2>&1 | Select-String -Pattern '^(error|warning)' -Context 0,8
"built: $((Get-Item "$PSScriptRoot\..\target\release\veyra.exe").LastWriteTime)"
