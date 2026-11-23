# Fast development hot-reload workflow for Herdr2 on Windows:
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1 ui
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1 web
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1 core
#   powershell -ExecutionPolicy Bypass -File scripts/dev.ps1 app
param(
    [Parameter(Position = 0)]
    [ValidateSet("ui", "web", "core", "app", "help")]
    [string]$Command = "help"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$env:HERDR2_DEV_DEFAULTS = Join-Path $Root "herdr-app\defaults"

function Find-AppBin {
    $candidates = @(
        (Join-Path $Root "herdr-app\target\debug\herdr2.exe"),
        (Join-Path $Root "herdr-app\target\release\herdr2.exe"),
        (Join-Path $env:LOCALAPPDATA "Programs\herdr2\herdr2.exe")
    )
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return $c }
    }
    $cmd = Get-Command herdr2 -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

switch ($Command) {
    "ui" {
        $bin = Find-AppBin
        if (-not $bin) {
            Write-Host "[dev-ui] No herdr2.exe found; building fast debug binary once..."
            Push-Location (Join-Path $Root "herdr-app")
            try { cargo build --locked } finally { Pop-Location }
            if ($LASTEXITCODE) { exit $LASTEXITCODE }
            $bin = Join-Path $Root "herdr-app\target\debug\herdr2.exe"
        }
        Write-Host "[dev-ui] Live hot-reload enabled for: $env:HERDR2_DEV_DEFAULTS"
        & $bin
    }

    "web" {
        Write-Host "[dev-web] Watching drove-web -> $Root\drove-web\dist"
        Push-Location (Join-Path $Root "drove-web")
        try {
            if (-not (Test-Path "node_modules")) { npm ci }
            npm run build -- --watch
        } finally { Pop-Location }
    }

    "core" {
        Write-Host "[dev-core] Fast incremental debug build (skipping drove-web rebuild)..."
        $env:H2_SKIP_WEB_BUILD = "1"
        Push-Location (Join-Path $Root "herdr-core")
        try { cargo build --locked --bin herdr2 } finally { Pop-Location }
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
        $built = Join-Path $Root "herdr-core\target\debug\herdr2.exe"
        $installed = Join-Path $env:LOCALAPPDATA "Programs\h2\h2.exe"
        if (Test-Path -LiteralPath $installed) {
            & $installed update --from $built --force
        } else {
            & $built server restart
        }
    }

    "app" {
        Write-Host "[dev-app] Fast incremental debug build (deps are opt-level=2)..."
        Push-Location (Join-Path $Root "herdr-app")
        try { cargo build --locked } finally { Pop-Location }
        if ($LASTEXITCODE) { exit $LASTEXITCODE }
        $bin = Join-Path $Root "herdr-app\target\debug\herdr2.exe"
        & $bin status 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "[dev-app] Restarting Herdr2 window cleanly (terminals stay alive in h2)..."
            & $bin do app.restart
        } else {
            & $bin
        }
    }

    default {
        Write-Host "Herdr2 Windows fast dev workflow:"
        Write-Host "  scripts/dev.ps1 ui    Zero-compile live reload for defaults/{mods,frame,layouts,themes,icons}"
        Write-Host "  scripts/dev.ps1 web   Live watch rebuild for drove-web/dist"
        Write-Host "  scripts/dev.ps1 core  Fast debug build + h2 update --from --force (zero-downtime)"
        Write-Host "  scripts/dev.ps1 app   Fast debug build + hot-restart window"
    }
}
