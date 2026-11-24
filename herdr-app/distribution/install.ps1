# Installs the Herdr2 app on Windows, and herdr2-core (`h2`) if there's none yet:
#
#   $env:HERDR2_RELEASES_URL="https://your-host"; irm "$env:HERDR2_RELEASES_URL/app/install.ps1" | iex
#
# The app installs into %LOCALAPPDATA%\Programs\herdr2 (added to user PATH for
# `herdr2`), with a Start menu shortcut. Running it again updates the app cleanly.

& {
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Fail($why) { throw "herdr2: $why" }

$releases = if ($env:HERDR2_RELEASES_URL) { $env:HERDR2_RELEASES_URL.TrimEnd("/") } elseif ($env:HERDR_RELEASES_URL) { $env:HERDR_RELEASES_URL.TrimEnd("/") } else { "https://github.com/herdr2/herdr2/releases/download/ota-latest" }
$isGhFlat = ($releases -like "*/releases/download/*") -or ($releases -like "*/releases/latest/download")
$appManifest = if ($isGhFlat) { "$releases/app-latest.json" } else { "$releases/app/latest.json" }
$coreInstaller = if ($isGhFlat) { "$releases/core-install.ps1" } else { "$releases/core/install.ps1" }

$dir = Join-Path $env:LOCALAPPDATA "Programs\herdr2"
$arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($arch -ne "AMD64") { Fail "no build for $arch Windows yet (x64 only)" }
$target = "windows-x86_64"
$tar = Join-Path $env:SystemRoot "System32\tar.exe"
if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue) -or -not (Test-Path $tar)) {
    Fail "needs curl.exe and tar.exe (Windows 10 1803 or newer)"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) "herdr2-install-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    curl.exe -fsSL $appManifest -o "$tmp\latest.json"
    if ($LASTEXITCODE) { Fail "cannot reach $releases" }
    $m = Get-Content -Raw "$tmp\latest.json" | ConvertFrom-Json
    $t = $m.targets.$target
    if (-not $t.url -or -not $t.sha256) { Fail "no build for $target yet" }

    Write-Host "Installing Herdr2 $($m.version) for $target..."
    curl.exe -fsSL $t.url -o "$tmp\app.tar.gz"
    if ($LASTEXITCODE) { Fail "download failed: $($t.url)" }
    $stream = [IO.File]::OpenRead("$tmp\app.tar.gz")
    try { $got = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($stream) | ForEach-Object { $_.ToString("x2") }) } finally { $stream.Close() }
    if ($got -ne $t.sha256) { Fail "the download is damaged (checksum mismatch); try again" }
    & $tar -xzf "$tmp\app.tar.gz" -C $tmp
    if ($LASTEXITCODE -or -not (Test-Path "$tmp\herdr2\herdr2.exe")) { Fail "the download holds no complete app" }

    New-Item -ItemType Directory -Force (Split-Path $dir) | Out-Null
    Remove-Item -Recurse -Force "$dir.old" -ErrorAction SilentlyContinue
    if (Test-Path $dir) { Rename-Item $dir "$dir.old" }
    Move-Item "$tmp\herdr2" $dir
    Remove-Item -Recurse -Force "$dir.old" -ErrorAction SilentlyContinue

    $exe = Join-Path $dir "herdr2.exe"
    $menu = Join-Path ([Environment]::GetFolderPath("Programs")) "Herdr2.lnk"
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($menu)
    $link.TargetPath = $exe
    $link.WorkingDirectory = $env:USERPROFILE
    $link.Description = "Terminals and coding agents that keep running"
    $link.Save()

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
    $raw = [string]$key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $listed = $raw -split ";" | Where-Object { $_.Trim().TrimEnd("\") -ieq $dir.TrimEnd("\") }
    if (-not $listed) {
        $kind = if ($key.GetValueNames() -contains "Path") { $key.GetValueKind("Path") } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue("Path", $(if ($raw) { "$($raw.TrimEnd(';'));$dir" } else { $dir }), $kind)
        [Environment]::SetEnvironmentVariable("HERDR2_INSTALLING", "1", "User")
        [Environment]::SetEnvironmentVariable("HERDR2_INSTALLING", $null, "User")
    }
    $key.Close()
    if (($env:Path -split ";") -notcontains $dir) { $env:Path += ";$dir" }

    $core = Join-Path $env:LOCALAPPDATA "Programs\h2\h2.exe"
    if (-not (Test-Path $core) -and -not (Get-Command h2 -ErrorAction SilentlyContinue)) {
        try {
            $env:HERDR2_RELEASES_URL = $releases
            Invoke-RestMethod $coreInstaller | Invoke-Expression
        } catch {
            Write-Host "herdr2-core didn't install; Herdr2 will offer it when it opens."
        }
    }

    Write-Host "Herdr2 is installed. Open it from the Start menu, or run: herdr2"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
}
