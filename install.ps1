# Install the claude-code-statusline on Windows.
#
# Usage (from PowerShell in the cloned repo):
#   .\install.ps1                  # build + place binary + patch settings.json
#   .\install.ps1 -WithAutobuild   # also register a Scheduled Task for auto-rebuild
#   .\install.ps1 -Uninstall       # remove the symlink + statusLine block + task
#   .\install.ps1 -Help            # show this header
#
# Idempotent: safe to re-run to update an existing install.
#
# What it does:
#   1. Verifies cargo is available
#   2. Builds the release binary (cargo build --release)
#   3. Places target\release\statusline.exe at $HOME\.claude\bin\cc-statusline.exe
#      via symlink when possible (Developer Mode), copy as fallback
#   4. Patches $HOME\.claude\settings.json with the statusLine block,
#      preserving every other key
#
# Note on the auto-rebuild add-on: on Windows we register a Scheduled Task
# running cargo-watch in the background. This is conceptually identical to
# the macOS LaunchAgent / Linux systemd unit; the underlying tool runs the
# same way (cargo-watch -x "build --release" -w src -w Cargo.toml).

param(
    [switch]$WithAutobuild,
    [switch]$Uninstall,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'

# --- Paths ------------------------------------------------------------------
$RepoDir   = $PSScriptRoot
$BinDir    = Join-Path $HOME '.claude\bin'
$Settings  = Join-Path $HOME '.claude\settings.json'
$BinTarget = Join-Path $BinDir 'cc-statusline.exe'
$BinSource = Join-Path $RepoDir 'target\release\statusline.exe'
$TaskName  = 'dev.abdellah.cc-statusline-autobuild'

# --- Help -------------------------------------------------------------------
if ($Help) {
    Get-Content $PSCommandPath | Select-Object -First 22 | ForEach-Object {
        $_ -replace '^# ?', ''
    }
    exit 0
}

# --- Uninstall path ---------------------------------------------------------
if ($Uninstall) {
    Write-Host "==> Uninstalling..."
    # Scheduled Task
    $task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($task) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Host "  removed Scheduled Task"
    }
    # Binary symlink/copy
    if (Test-Path $BinTarget) {
        Remove-Item $BinTarget -Force
        Write-Host "  removed $BinTarget"
    }
    # settings.json statusLine block
    if (Test-Path $Settings) {
        $data = Get-Content $Settings -Raw | ConvertFrom-Json
        if ($data.PSObject.Properties.Name -contains 'statusLine') {
            $cmd = $data.statusLine.command
            if ($cmd -like '*cc-statusline*') {
                $data.PSObject.Properties.Remove('statusLine')
                $data | ConvertTo-Json -Depth 50 | Set-Content $Settings -NoNewline
                Add-Content $Settings "`n"
                Write-Host "  removed statusLine from settings.json"
            }
        }
    }
    Write-Host "==> Uninstalled."
    exit 0
}

# --- Pre-flight -------------------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host @"
Error: cargo not found.

Install Rust first:
  winget install Rustlang.Rustup
  # or use rustup-init.exe from https://rustup.rs

Then re-run this script.
"@ -ForegroundColor Red
    exit 1
}

# --- Build ------------------------------------------------------------------
Write-Host "==> Building release binary (libgit2 vendored — first build ~30-60s)..."
Push-Location $RepoDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

if (-not (Test-Path $BinSource)) {
    Write-Host "Error: expected binary at $BinSource not found after build" -ForegroundColor Red
    exit 1
}

# --- Place binary (symlink preferred, copy as fallback) ---------------------
if (-not (Test-Path $BinDir)) {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
}

if (Test-Path $BinTarget) { Remove-Item $BinTarget -Force }

try {
    # Symlink requires Developer Mode OR admin. Try it first — gives "rebuild
    # is live" semantics matching the macOS/Linux installers.
    New-Item -ItemType SymbolicLink -Path $BinTarget -Target $BinSource | Out-Null
    Write-Host "==> Symlinked  $BinTarget"
    Write-Host "             → $BinSource"
} catch {
    Copy-Item -Path $BinSource -Destination $BinTarget -Force
    Write-Host "==> Copied     $BinSource"
    Write-Host "             → $BinTarget"
    Write-Host "    (symlink failed — enable Developer Mode to get live-rebuild updates;"
    Write-Host "     otherwise re-run install.ps1 after rebuilds)"
}

# --- Patch settings.json ----------------------------------------------------
if (-not (Test-Path (Split-Path $Settings))) {
    New-Item -ItemType Directory -Path (Split-Path $Settings) -Force | Out-Null
}
if (-not (Test-Path $Settings)) {
    Set-Content -Path $Settings -Value '{}' -NoNewline
}

$data = Get-Content $Settings -Raw | ConvertFrom-Json

# Build the statusLine object — overwrite (idempotent), preserve other keys.
$newStatusLine = [PSCustomObject]@{
    type    = 'command'
    command = '$HOME/.claude/bin/cc-statusline.exe'
}

if ($data.PSObject.Properties.Name -contains 'statusLine') {
    $data.statusLine = $newStatusLine
} else {
    $data | Add-Member -NotePropertyName 'statusLine' -NotePropertyValue $newStatusLine -Force
}

$data | ConvertTo-Json -Depth 50 | Set-Content $Settings -NoNewline
Add-Content $Settings "`n"
Write-Host "==> Patched   $Settings"

# --- Optional: Scheduled Task for auto-rebuild ------------------------------
if ($WithAutobuild) {
    $cargoWatch = Join-Path $HOME '.cargo\bin\cargo-watch.exe'
    if (-not (Test-Path $cargoWatch) -and -not (Get-Command cargo-watch -ErrorAction SilentlyContinue)) {
        Write-Host "==> Installing cargo-watch (one-time, ~3min)..."
        cargo install cargo-watch
        if ($LASTEXITCODE -ne 0) { throw "cargo install cargo-watch failed" }
    }
    if (-not (Test-Path $cargoWatch)) {
        $cargoWatch = (Get-Command cargo-watch).Source
    }

    # Unregister any previous version so this is idempotent.
    $existing = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if ($existing) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    }

    $action = New-ScheduledTaskAction `
        -Execute $cargoWatch `
        -Argument '-x "build --release" -w src -w Cargo.toml' `
        -WorkingDirectory $RepoDir

    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME

    $settings_ = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit (New-TimeSpan -Hours 0) `
        -RestartCount 999 `
        -RestartInterval (New-TimeSpan -Minutes 1)

    Register-ScheduledTask `
        -TaskName $TaskName `
        -Action $action `
        -Trigger $trigger `
        -Settings $settings_ `
        -Description 'Claude Code statusline auto-rebuild' | Out-Null

    Start-ScheduledTask -TaskName $TaskName
    Write-Host "==> Scheduled Task registered ($TaskName)"
    Write-Host "    cargo-watch runs in the background; binary rebuilds on src/ or Cargo.toml edits."
}

# --- Done -------------------------------------------------------------------
Write-Host @"

==> Installed.

The next Claude Code turn will use the statusline. If it doesn't appear,
restart Claude Code or start a new session.

Update later: cd into this repo, git pull, .\install.ps1
Uninstall   : .\install.ps1 -Uninstall
"@
