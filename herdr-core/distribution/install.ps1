# Installs herdr2 core (`h2`) on Windows, or updates it from GitHub Releases / a self-hosted server:
#
#   irm https://github.com/herdr2/herdr2/releases/download/ota-latest/core-install.ps1 | iex
#
# Into %LOCALAPPDATA%\Programs\h2 (herdr2-core.exe + h2.exe), added to your PATH,
# completely separate from the Herdr2 desktop app in %LOCALAPPDATA%\Programs\herdr2. No admin needed.
# H2_VERSION=0.2.0 picks a version; HERDR2_RELEASES_URL overrides the release base URL;
# H2_INSTALL_DIR installs elsewhere.

& {
$zip, $session = $args
if ($session) { $env:H2_SESSION = $session }
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Fail($why) { throw "h2: $why" }

$releases = if ($env:HERDR2_RELEASES_URL) { $env:HERDR2_RELEASES_URL.TrimEnd("/") } else { "https://github.com/herdr2/herdr2/releases/download/ota-latest" }
$dir = if ($env:H2_INSTALL_DIR) { $env:H2_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\h2" }
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne "AMD64") { Fail "no build for $arch Windows yet (x64 only)" }
$target = "windows-x86_64"
$tar = Join-Path $env:SystemRoot "System32\tar.exe"
if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue) -or -not (Test-Path $tar)) {
    Fail "needs curl.exe and tar.exe (Windows 10 1803 or newer)"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) "h2-install-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
  if ($zip) {
    Copy-Item $zip "$tmp\h2.zip"
  } else {
    $isGhFlat = ($releases -like "*/releases/download/*") -or ($releases -like "*/releases/latest/download")
    $manifest = if ($isGhFlat) {
        if ($env:H2_VERSION) { "$releases/core-$($env:H2_VERSION)-manifest.json" } else { "$releases/core-latest.json" }
    } else {
        if ($env:H2_VERSION) { "$releases/core/$($env:H2_VERSION)/manifest.json" } else { "$releases/core/latest.json" }
    }
    curl.exe -fsSL $manifest -o "$tmp\manifest.json"
    if ($LASTEXITCODE) { Fail "cannot reach $manifest" }
    $m = Get-Content -Raw "$tmp\manifest.json" | ConvertFrom-Json
    $t = $m.targets.$target
    if (-not $t.url -or -not $t.sha256) { Fail "no build for $target in $manifest" }

    $src = if ((Test-Path (Join-Path $dir "h2.exe")) -or (Test-Path (Join-Path $dir "herdr2-core.exe"))) { "update" } else { "install" }
    $plain = $t.url.StartsWith("$releases/", [StringComparison]::Ordinal) -and $t.url.IndexOfAny([char[]]"?#") -lt 0
    $download = if ($plain) { "$($t.url)?src=$src" } else { $t.url }
    curl.exe -fsSL $download -o "$tmp\h2.zip"
    if ($LASTEXITCODE) { Fail "download failed: $($t.url)" }
    $stream = [IO.File]::OpenRead("$tmp\h2.zip")
    try { $got = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) } finally { $stream.Close() }
    if ($got -ne $t.sha256) { Fail "the download is damaged (checksum mismatch); try again" }
  }
    New-Item -ItemType Directory -Force "$tmp\h2" | Out-Null
    & $tar -xf "$tmp\h2.zip" -C "$tmp\h2"
    $new = "$tmp\h2\herdr2.exe"
    if (-not (Test-Path $new) -and (Test-Path "$tmp\h2\h2.exe")) { $new = "$tmp\h2\h2.exe" }
    if ($LASTEXITCODE -or -not (Test-Path $new)) { Fail "the download holds no complete h2" }
    $version = (& $new --version) -replace "^h2 ", ""
    if ($LASTEXITCODE) { Fail "the downloaded h2 does not run on this machine" }

    New-Item -ItemType Directory -Force $dir | Out-Null
    $exe = Join-Path $dir "herdr2-core.exe"
    $h2Exe = Join-Path $dir "h2.exe"
    if (Test-Path $exe) {
        & $exe update --from $new | Out-Null
        if ($LASTEXITCODE) { Fail "updating $exe failed (see above)" }
        Write-Host "h2 updated to $version."
    } elseif (Test-Path $h2Exe) {
        & $h2Exe update --from $new | Out-Null
        if ($LASTEXITCODE) { Fail "updating $h2Exe failed (see above)" }
        Copy-Item -Force $h2Exe $exe
        Write-Host "h2 updated to $version."
    } else {
        Copy-Item -Force $new $exe
        Write-Host "h2 $version installed into $dir."
    }
    Copy-Item -Recurse -Force "$tmp\h2\THIRD-PARTY-NOTICES" $dir -ErrorAction SilentlyContinue
    if (Test-Path $h2Exe) { Remove-Item -Force $h2Exe -ErrorAction SilentlyContinue }
    try {
        New-Item -ItemType HardLink -Path $h2Exe -Target $exe | Out-Null
    } catch {
        Copy-Item -Force $exe $h2Exe
    }
    try { & $exe skill sync 2>$null | Out-Null } catch {}

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
    $raw = [string]$key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $listed = $raw -split ";" | Where-Object { $_.Trim().TrimEnd("\") -ieq $dir.TrimEnd("\") }
    if (-not $listed) {
        $kind = if ($key.GetValueNames() -contains "Path") { $key.GetValueKind("Path") } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue("Path", $(if ($raw) { "$($raw.TrimEnd(';'));$dir" } else { $dir }), $kind)
        [Environment]::SetEnvironmentVariable("H2_INSTALLING", "1", "User")
        [Environment]::SetEnvironmentVariable("H2_INSTALLING", $null, "User")
        Write-Host "Added $dir to your PATH: new shells have h2."
    }
    $key.Close()
    if (($env:Path -split ";") -notcontains $dir) { $env:Path += ";$dir" }
    Write-Host "Skills: h2 skill install --all. Agents: h2 --skill. People: h2 --help."
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    if ($zip) { Remove-Item -Force $zip, $PSCommandPath -ErrorAction SilentlyContinue }
}
} @args
