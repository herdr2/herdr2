# Builds this checkout and installs it with distribution/install.ps1.
$ErrorActionPreference = "Stop"
Set-Location "$PSScriptRoot\.."
cargo build --release --locked
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "h2-local-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
Copy-Item distribution\install.ps1 $tmp
& "$env:SystemRoot\System32\tar.exe" -a -cf "$tmp\h2.zip" -C target\release herdr2.exe
if ($LASTEXITCODE) { exit $LASTEXITCODE }
powershell -NoProfile -ExecutionPolicy Bypass -File "$tmp\install.ps1" "$tmp\h2.zip"
$code = $LASTEXITCODE
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
exit $code
