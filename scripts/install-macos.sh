#!/usr/bin/env bash
# Install RustFlow on macOS from this source tree.
# Safe to run again: existing config and token are kept.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CONFIG_DIR="$HOME/.config/rustflow"
APP="/Applications/rustflow.app"
IDENTITY="rustflow local signing"

say()  { printf '\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*"; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
ask()  { local reply; read -r -p "$1 [$2]: " reply; printf '%s' "${reply:-$2}"; }

[ "$(uname -s)" = Darwin ] || die "this installer is for macOS; use scripts/install-linux.sh on Linux"

say "Checking requirements"
xcode-select -p >/dev/null 2>&1 || die "Xcode Command Line Tools are missing. Install them with: xcode-select --install"
command -v cargo >/dev/null || die "Rust is not installed. Install it with:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

if ! security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
  echo
  echo "macOS forgets the Accessibility permission every time an unsigned app is rebuilt."
  echo "A local signing certificate (stored only in your login keychain) avoids that."
  if [ "$(ask "Create it now?" "Y")" != "n" ]; then
    "$ROOT/packaging/macos/make-signing-cert.sh"
  fi
fi

say "Building $APP"
osascript -e 'quit app "rustflow"' >/dev/null 2>&1 || true
"$ROOT/packaging/macos/build-app.sh"

mkdir -p "$CONFIG_DIR"
if [ -f "$CONFIG_DIR/config.toml" ]; then
  say "Keeping existing $CONFIG_DIR/config.toml"
else
  say "Setting up $CONFIG_DIR/config.toml"
  peer=$(ask "Linux machine's address (IP or hostname)" "192.168.1.20")
  peer_name=$(ask "Name to show for it" "Linux")
  side=$(ask "Which side of this Mac's screen is it on? (left/right)" "left")
  case "$side" in left|right) ;; *) die "please answer left or right" ;; esac
  echo "File copy/paste uses scp, so this Mac must be able to run 'ssh <host>' to it without a password."
  ssh_host=$(ask "SSH host for it (empty to skip file copy/paste)" "$peer")
  sed -e "s|@PEER@|$peer|" -e "s|@PEER_NAME@|$peer_name|" -e "s|@PEER_SIDE@|$side|" -e "s|@SSH_HOST@|$ssh_host|" \
    "$ROOT/packaging/config/macos.toml" > "$CONFIG_DIR/config.toml"
fi

if [ -f "$CONFIG_DIR/token" ]; then
  say "Keeping existing shared token"
else
  echo
  echo "Both machines must share the same secret token."
  read -r -p "Paste the token printed by the Linux installer, or press Enter to create a new one: " token
  if [ -z "$token" ]; then
    token=$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 32)
    echo
    echo "New token (enter it when installing on Linux):"
    echo "    $token"
    echo
  fi
  (umask 077 && printf '%s\n' "$token" > "$CONFIG_DIR/token")
fi

say "Starting at login"
if osascript -e 'tell application "System Events" to get the name of every login item' 2>/dev/null | grep -q rustflow; then
  echo "already a login item"
else
  osascript -e "tell application \"System Events\" to make login item at end with properties {path:\"$APP\", hidden:true}" >/dev/null
fi

say "Starting RustFlow"
open "$APP"

cat <<EOF

Done. Next:
  * Allow RustFlow when macOS asks about the local network.
  * Turn RustFlow on in System Settings > Privacy & Security > Accessibility
    (needed to share the keyboard and mouse).
  * Push the pointer off the $( [ -f "$CONFIG_DIR/config.toml" ] && grep -E '^peer_side' "$CONFIG_DIR/config.toml" | cut -d'"' -f2 || echo left) edge of the screen to reach Linux.
  * Settings: $CONFIG_DIR/config.toml   Log: ~/Library/Logs/rustflow/rustflow.log
EOF
