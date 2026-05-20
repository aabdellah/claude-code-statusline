#!/usr/bin/env bash
# Install the claude-code-statusline on this machine.
#
# Supported: macOS (Darwin), Linux. Windows users: see install.ps1.
#
# Idempotent: safe to re-run to update an existing install.
#
# What it does:
#   1. Verifies cargo is available
#   2. Builds the release binary
#   3. Symlinks ~/.claude/bin/cc-statusline → target/release/statusline
#   4. Patches ~/.claude/settings.json so CC uses the binary
#
# Optional add-on (auto-rebuild on source edits):
#   ./install.sh --with-autobuild
#     • macOS uses launchd (LaunchAgent)
#     • Linux uses systemd user unit
#
# Uninstall:
#   ./install.sh --uninstall

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.claude/bin"
SETTINGS="$HOME/.claude/settings.json"
SYMLINK="$BIN_DIR/cc-statusline"

# --- OS detection -----------------------------------------------------------
OS="$(uname -s)"
case "$OS" in
  Darwin)
    AUTOBUILD_TYPE="launchd"
    LAUNCHAGENT_LABEL="dev.abdellah.cc-statusline-autobuild"
    LAUNCHAGENT_PATH="$HOME/Library/LaunchAgents/${LAUNCHAGENT_LABEL}.plist"
    ;;
  Linux)
    AUTOBUILD_TYPE="systemd"
    SYSTEMD_UNIT_NAME="dev.abdellah.cc-statusline-autobuild.service"
    SYSTEMD_UNIT_PATH="$HOME/.config/systemd/user/${SYSTEMD_UNIT_NAME}"
    ;;
  *)
    echo "Unsupported OS: $OS (only Darwin / Linux supported in install.sh)" >&2
    echo "Windows users: run install.ps1 from PowerShell instead." >&2
    exit 1
    ;;
esac

# --- Flags ------------------------------------------------------------------
WITH_AUTOBUILD=0
DO_UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --with-autobuild) WITH_AUTOBUILD=1 ;;
    --uninstall)      DO_UNINSTALL=1 ;;
    -h|--help)
      sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# --- Uninstall path ---------------------------------------------------------
if [ "$DO_UNINSTALL" = "1" ]; then
  echo "==> Uninstalling..."
  case "$AUTOBUILD_TYPE" in
    launchd)
      if [ -e "$LAUNCHAGENT_PATH" ]; then
        launchctl unload "$LAUNCHAGENT_PATH" 2>/dev/null || true
        rm -f "$LAUNCHAGENT_PATH"
        echo "  removed LaunchAgent"
      fi
      ;;
    systemd)
      if [ -e "$SYSTEMD_UNIT_PATH" ]; then
        systemctl --user disable --now "$SYSTEMD_UNIT_NAME" 2>/dev/null || true
        rm -f "$SYSTEMD_UNIT_PATH"
        systemctl --user daemon-reload 2>/dev/null || true
        echo "  removed systemd user unit"
      fi
      ;;
  esac
  if [ -L "$SYMLINK" ] || [ -e "$SYMLINK" ]; then
    rm -f "$SYMLINK"
    echo "  removed symlink at $SYMLINK"
  fi
  if [ -f "$SETTINGS" ]; then
    SETTINGS_PATH="$SETTINGS" python3 <<'PYEOF'
import json, os
p = os.environ["SETTINGS_PATH"]
with open(p) as f: d = json.load(f)
if "statusLine" in d:
    cmd = d["statusLine"].get("command", "")
    if "cc-statusline" in cmd:
        del d["statusLine"]
        with open(p, "w") as f: json.dump(d, f, indent=2)
        f = open(p, "a"); f.write("\n"); f.close()
        print("  removed statusLine from settings.json")
PYEOF
  fi
  echo "==> Uninstalled."
  exit 0
fi

# --- Pre-flight -------------------------------------------------------------
if ! command -v cargo >/dev/null; then
  cat >&2 <<MSG
Error: cargo not found.

Install Rust first:
$(if [ "$OS" = "Darwin" ]; then
  echo "  brew install rust                                             # easiest on macOS"
elif [ "$OS" = "Linux" ]; then
  echo "  # Debian/Ubuntu:  sudo apt install rustc cargo"
  echo "  # Fedora:         sudo dnf install rust cargo"
  echo "  # Arch:           sudo pacman -S rust"
fi)
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # cross-platform

Then re-run this script.
MSG
  exit 1
fi

if ! command -v python3 >/dev/null; then
  echo "Error: python3 not found (used to patch settings.json)" >&2
  case "$OS" in
    Darwin) echo "  → bundled with macOS 12.3+, or 'brew install python'" >&2 ;;
    Linux)  echo "  → install via your distro: apt install python3 / dnf install python3 / pacman -S python" >&2 ;;
  esac
  exit 1
fi

# On Linux, libgit2 vendored needs a C compiler — sanity-check.
if [ "$OS" = "Linux" ] && ! command -v cc >/dev/null && ! command -v gcc >/dev/null; then
  echo "Warning: no C compiler found (cc / gcc). The vendored libgit2 build will fail."
  echo "  Debian/Ubuntu: sudo apt install build-essential"
  echo "  Fedora:        sudo dnf groupinstall 'Development Tools'"
  echo "  Arch:          sudo pacman -S base-devel"
  echo "Proceeding anyway in case you have an alternative toolchain."
fi

# --- Build ------------------------------------------------------------------
echo "==> Building release binary (libgit2 vendored — first build ~30-60s)..."
(cd "$REPO_DIR" && cargo build --release)

# --- Symlink ----------------------------------------------------------------
mkdir -p "$BIN_DIR"
ln -sf "$REPO_DIR/target/release/statusline" "$SYMLINK"
echo "==> Symlinked  $SYMLINK"
echo "             → $REPO_DIR/target/release/statusline"

# --- Patch settings.json ----------------------------------------------------
mkdir -p "$(dirname "$SETTINGS")"
if [ ! -f "$SETTINGS" ]; then
  echo '{}' > "$SETTINGS"
fi

SETTINGS_PATH="$SETTINGS" python3 <<'PYEOF'
import json, os
p = os.environ["SETTINGS_PATH"]
with open(p) as f:
    data = json.load(f)

# Idempotent — overwrite the statusLine block; leave every other key untouched.
data["statusLine"] = {
    "type": "command",
    "command": "$HOME/.claude/bin/cc-statusline",
}

with open(p, "w") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
print(f"==> Patched   {p}")
PYEOF

# --- Optional: auto-rebuild on source edits ---------------------------------
if [ "$WITH_AUTOBUILD" = "1" ]; then
  if ! command -v cargo-watch >/dev/null && [ ! -x "$HOME/.cargo/bin/cargo-watch" ]; then
    echo "==> Installing cargo-watch (one-time, ~3min)..."
    cargo install cargo-watch
  fi
  CARGO_WATCH_BIN="$HOME/.cargo/bin/cargo-watch"
  if [ ! -x "$CARGO_WATCH_BIN" ]; then
    CARGO_WATCH_BIN="$(command -v cargo-watch)"
  fi

  case "$AUTOBUILD_TYPE" in
    launchd)
      mkdir -p "$(dirname "$LAUNCHAGENT_PATH")"
      cat > "$LAUNCHAGENT_PATH" <<XML
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${LAUNCHAGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${CARGO_WATCH_BIN}</string>
        <string>-x</string>
        <string>build --release</string>
        <string>-w</string>
        <string>src</string>
        <string>-w</string>
        <string>Cargo.toml</string>
    </array>
    <key>WorkingDirectory</key>
    <string>${REPO_DIR}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>/tmp/cc-statusline-autobuild.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/cc-statusline-autobuild.err</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin</string>
        <key>HOME</key>
        <string>${HOME}</string>
    </dict>
</dict>
</plist>
XML
      launchctl unload "$LAUNCHAGENT_PATH" 2>/dev/null || true
      launchctl load "$LAUNCHAGENT_PATH"
      echo "==> LaunchAgent loaded  ($LAUNCHAGENT_LABEL)"
      ;;

    systemd)
      mkdir -p "$(dirname "$SYSTEMD_UNIT_PATH")"
      cat > "$SYSTEMD_UNIT_PATH" <<UNIT
[Unit]
Description=Claude Code statusline auto-rebuild
After=default.target

[Service]
Type=simple
ExecStart=${CARGO_WATCH_BIN} -x "build --release" -w src -w Cargo.toml
WorkingDirectory=${REPO_DIR}
Environment=PATH=${HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin
Environment=HOME=${HOME}
Restart=always
RestartSec=10
StandardOutput=append:/tmp/cc-statusline-autobuild.log
StandardError=append:/tmp/cc-statusline-autobuild.err

[Install]
WantedBy=default.target
UNIT
      systemctl --user daemon-reload
      systemctl --user enable --now "$SYSTEMD_UNIT_NAME"
      echo "==> systemd user unit loaded  ($SYSTEMD_UNIT_NAME)"
      ;;
  esac
  echo "    cargo-watch runs in the background; binary rebuilds on src/ or Cargo.toml edits."
fi

# --- Done -------------------------------------------------------------------
cat <<DONE

==> Installed.

The next Claude Code turn will use the statusline. If it doesn't appear,
restart Claude Code or start a new session.

Update later: cd into this repo, git pull, ./install.sh
Uninstall   : ./install.sh --uninstall
DONE
