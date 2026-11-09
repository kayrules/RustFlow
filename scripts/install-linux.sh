#!/usr/bin/env bash
# Install RustFlow on Linux (GNOME on Wayland) from this source tree.
# Safe to run again: existing config and token are kept.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CONFIG_DIR="$HOME/.config/rustflow"
BIN_DIR="$HOME/.local/bin"
APP_ID="dev.rustflow.Rustflow"
DESKTOP_FILE="$HOME/.local/share/applications/$APP_ID.desktop"
AUTOSTART_FILE="$HOME/.config/autostart/$APP_ID.desktop"
UDEV_RULE="/etc/udev/rules.d/60-rustflow-uinput.rules"

say()  { printf '\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*"; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "this installer is for Linux; use scripts/install-macos.sh on a Mac"

say "Checking requirements"
command -v cargo >/dev/null || die "Rust is not installed. Install it with:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
command -v cc >/dev/null || die "a C compiler is missing. On Ubuntu: sudo apt install build-essential"
[ "${XDG_SESSION_TYPE:-}" = wayland ] || warn "this session is not Wayland; clipboard sync needs GNOME on Wayland"
command -v zenity >/dev/null || warn "zenity not found; no progress window for large file pastes (sudo apt install zenity)"
if command -v gnome-extensions >/dev/null; then
  if ! gnome-extensions list --enabled 2>/dev/null | grep -q appindicator; then
    warn "no AppIndicator extension is enabled, so the tray icon won't show.
         On Ubuntu: gnome-extensions enable ubuntu-appindicators@ubuntu.com"
  fi
fi

say "Building (release)"
(cd "$ROOT" && cargo build --release --locked)

say "Installing to $BIN_DIR/rustflow"
mkdir -p "$BIN_DIR"
# Replace instead of overwriting in place, so a running copy keeps working.
install -m 755 "$ROOT/target/release/rustflow" "$BIN_DIR/rustflow.new"
mv -f "$BIN_DIR/rustflow.new" "$BIN_DIR/rustflow"

say "Adding the app menu entry and starting at login"
mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$AUTOSTART_FILE")"
sed "s|^Exec=.*|Exec=$BIN_DIR/rustflow|" "$ROOT/packaging/linux/$APP_ID.desktop" > "$DESKTOP_FILE"
cp "$DESKTOP_FILE" "$AUTOSTART_FILE"

if cmp -s "$ROOT/packaging/linux/60-rustflow-uinput.rules" "$UDEV_RULE"; then
  say "Keyboard/mouse access (/dev/uinput) already set up"
else
  say "Allowing your desktop session to create a virtual keyboard and mouse (needs sudo)"
  sudo install -m 644 "$ROOT/packaging/linux/60-rustflow-uinput.rules" "$UDEV_RULE"
  sudo udevadm control --reload
  sudo udevadm trigger --name-match=uinput --action=change
fi

mkdir -p "$CONFIG_DIR"
if [ -f "$CONFIG_DIR/config.toml" ]; then
  say "Keeping existing $CONFIG_DIR/config.toml"
else
  say "Writing $CONFIG_DIR/config.toml"
  cp "$ROOT/packaging/config/linux.toml" "$CONFIG_DIR/config.toml"
fi

if [ -f "$CONFIG_DIR/token" ]; then
  say "Keeping existing shared token"
else
  echo
  echo "Both machines must share the same secret token."
  read -r -p "Paste the token from your Mac, or press Enter to create a new one: " token
  if [ -z "$token" ]; then
    token=$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 32)
    echo
    echo "New token (enter it when installing on the Mac):"
    echo "    $token"
    echo
  fi
  (umask 077 && printf '%s\n' "$token" > "$CONFIG_DIR/token")
fi

say "Starting RustFlow"
pkill -x rustflow 2>/dev/null || true
if command -v gtk-launch >/dev/null; then
  gtk-launch "$APP_ID" >/dev/null 2>&1 || nohup "$BIN_DIR/rustflow" >/dev/null 2>&1 &
else
  nohup "$BIN_DIR/rustflow" >/dev/null 2>&1 &
fi

cat <<EOF

Done. Next:
  * The first start shows a "Remote Desktop" prompt: click Share.
    (RustFlow needs it for the clipboard; it is remembered afterwards.)
  * Install RustFlow on your Mac with scripts/install-macos.sh.
  * Settings: $CONFIG_DIR/config.toml   Log: ~/.local/state/rustflow/rustflow.log
EOF
