# RustFlow

Share one keyboard and mouse between a Mac and a Linux PC, with clipboard
sync, file copy/paste and Wake-on-LAN. Written in Rust.

RustFlow started as a replacement for [Deskflow](https://github.com/deskflow/deskflow)
on a desk with room for only one keyboard. It covers what Deskflow could not
do there: wake a sleeping Linux PC, share the clipboard on GNOME Wayland, and
type into the Linux lock screen.

> **Status:** early, personal project. It works daily on one Mac (macOS 26)
> and one Ubuntu 26.04 PC (GNOME, Wayland). Expect rough edges elsewhere.

## What it does

- **Keyboard and mouse sharing (Mac → Linux).** Push the pointer off the
  edge of the Mac screen and it appears on Linux; keyboard, clicks, scrolling
  and trackpad scrolling follow. Push it back to return.
  - Input on Linux goes through `/dev/uinput`, so Linux sees a real keyboard
    and mouse. It works everywhere, **including the lock screen**, and moving
    the pointer over wakes a blanked screen.
  - `Cmd+C` / `Cmd+V` become `Ctrl+C` / `Ctrl+V` on Linux (configurable).
  - `Ctrl+Option+Cmd` brings the pointer back to the Mac at any time.
- **Clipboard sync (both ways, text).** Copy on one machine, paste on the
  other. On GNOME Wayland this uses the official Clipboard portal, so it works
  for a background app without hacks.
- **File copy/paste (Mac → Linux).** Copy files in Finder, paste them in
  GNOME Files (folders should work too but are untested). Only the file names cross when you copy; the content is
  sent with `scp` when you actually paste. A progress window appears for slow
  transfers.
- **Wake.** The menu item *Wake &lt;peer&gt;* sends a Wake-on-LAN packet and,
  once connected, turns the Linux screen on and (optionally) unlocks it.
- **Optional unlock on enter.** Linux can unlock itself when the Mac pointer
  moves onto it.
- **Tray apps on both sides** showing connection, clipboard and input status,
  with toggles for clipboard sync and input sharing.
- **Reconnects by itself** when either machine sleeps, wakes or restarts.

## Limitations

- **Mac controls Linux only.** Input is shared from the Mac to Linux, not the
  other way. Only two machines, one Mac and one Linux PC.
- **Linux must run GNOME on Wayland** (tested on Ubuntu 26.04). Clipboard sync
  needs the xdg-desktop-portal *RemoteDesktop* and *Clipboard* interfaces;
  other desktops are untested.
- **The clipboard pauses while Linux is locked.** GNOME refuses portal
  sessions on the lock screen; sync resumes by itself after unlocking.
- **File paste is Mac → Linux only**, and it needs password-less SSH from the
  Mac to Linux. Pasting Linux files into Finder is not done yet.
- **Text clipboard only**: images and rich text are not synced.
- **No encryption.** The connection is plain TCP authenticated with a shared
  token. Use it on a trusted LAN or over a VPN such as Tailscale. File
  transfers use SSH and are encrypted.
- **One monitor per side assumed.** Several Linux monitors are treated as one
  wide screen; on the Mac, the leftmost/rightmost display is the edge.
- Media keys (volume, brightness) are not forwarded.
- Wake-on-LAN needs a wired network card on the Linux PC with WoL enabled in
  the BIOS and the OS.

## Requirements

**Mac**
- macOS 12 or newer (developed on macOS 26)
- Xcode Command Line Tools: `xcode-select --install`
- Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

**Linux**
- GNOME on Wayland (Ubuntu 24.04+ recommended)
- Build tools: `sudo apt install build-essential`
- Rust (same command as above)
- For the tray icon: the AppIndicator extension
  (`gnome-extensions enable ubuntu-appindicators@ubuntu.com` on Ubuntu)
- Optional: `zenity` for the paste progress window

**Network**
- Both machines on the same network (or a VPN), TCP port **24801** reachable
  on the Linux machine.
- For file paste: `ssh <linux-host>` from the Mac must work without a
  password (SSH key).

## Installation

Install on **Linux first**: its installer can create the shared token that the
Mac installer then asks for.

### 1. Linux

```bash
git clone https://github.com/kayrules/RustFlow.git
cd RustFlow
./scripts/install-linux.sh
```

The installer:
1. builds RustFlow and installs it to `~/.local/bin/rustflow`;
2. adds it to the app menu and to autostart;
3. installs a udev rule (asks for `sudo`) that lets your desktop session
   create the virtual keyboard and mouse;
4. writes `~/.config/rustflow/config.toml`;
5. asks for the shared token or creates one — **copy it for the Mac**;
6. starts RustFlow.

On first start GNOME shows a **Remote Desktop** prompt: click **Share**. It is
needed for clipboard access and is remembered afterwards.

### 2. Mac

```bash
git clone https://github.com/kayrules/RustFlow.git
cd RustFlow
./scripts/install-macos.sh
```

The installer:
1. offers to create a local code-signing certificate (recommended: otherwise
   macOS forgets the Accessibility permission on every rebuild);
2. builds `/Applications/rustflow.app` (menu bar only, no Dock icon);
3. asks for the Linux machine's address, which side of the screen it sits on,
   and its SSH host, then writes `~/.config/rustflow/config.toml`;
4. asks for the shared token from the Linux installer;
5. adds RustFlow to Login Items and starts it.

Then:
- allow RustFlow when macOS asks about the **local network**;
- turn RustFlow on in **System Settings › Privacy & Security › Accessibility**.

### Manual install

If you prefer not to run the scripts, see what they do — they are short:
[`scripts/install-linux.sh`](scripts/install-linux.sh),
[`scripts/install-macos.sh`](scripts/install-macos.sh). In short: build with
`cargo build --release`, put the same random token (16+ characters) in
`~/.config/rustflow/token` on both machines, copy a config from
[`packaging/config/`](packaging/config/), and run `rustflow` with no
arguments.

## Configuration

`~/.config/rustflow/config.toml` on each machine.

| Key | Where | Meaning |
|---|---|---|
| `role` | both | `"connect"` (Mac) or `"serve"` (Linux) |
| `peer` | Mac | Linux address, e.g. `"192.168.1.20"` (port 24801 by default) |
| `listen` | Linux | Address to listen on, default `"0.0.0.0:24801"` |
| `peer_name` | both | Name shown in the menu |
| `allow_remote_unlock` | Linux | Let the Mac's *Wake* item unlock this screen |
| `unlock_on_enter` | Linux | Unlock when the Mac pointer arrives |
| `[input] peer_side` | Mac | `"left"` or `"right"` of the Mac screen |
| `[input] swap_cmd_ctrl` | Mac | Send Cmd as Ctrl and Control as Super (default `true`) |
| `[input] speed` | Mac | Pointer speed on Linux (default `1.0`) |
| `[input] enabled` | Mac | Share keyboard and mouse (default `true`) |
| `[files] ssh_host` | Mac | SSH host of Linux for file paste; empty turns it off |
| `[wol] mac`, `[wol] broadcast` | Mac | Wake-on-LAN target and LAN broadcast address |

Restart RustFlow after editing (menu › Quit, then open it again).

Logs: `~/Library/Logs/rustflow/rustflow.log` (Mac),
`~/.local/state/rustflow/rustflow.log` (Linux).

### Command line

```text
rustflow                      run the tray app (default)
rustflow wake                 send the Wake-on-LAN packet from config.toml
rustflow clip serve|connect   clipboard sync without the tray
```

## Security notes

- Anyone who has the token can send keyboard input to the Linux machine and
  read/write both clipboards. Keep `~/.config/rustflow/token` private
  (the installers create it with mode 600).
- `allow_remote_unlock` and `unlock_on_enter` let the Mac unlock Linux. With
  either on, a locked Mac is what protects the Linux PC.
- The udev rule gives the logged-in desktop user access to `/dev/uinput`,
  so any program running as that user can create virtual input devices.

## Troubleshooting

- **Pointer won't cross to Linux:** check the menu shows *Mouse & keyboard:
  ready*. If it asks for Accessibility, enable RustFlow there (remove and
  re-add it after a rebuild if you skipped the signing certificate).
- **"No route to host" in the Mac log:** allow RustFlow in *System Settings ›
  Privacy & Security › Local Network*.
- **Clipboard error on Linux:** usually the screen is locked; it retries every
  10 s. If the Remote Desktop prompt was refused, restart RustFlow and click
  *Share*.
- **No tray icon on Linux:** enable the AppIndicator extension.
- **Linux says "no access to /dev/uinput":** the udev rule isn't active; run
  the installer again or log out and back in.

## Building from source

```bash
cargo build --release          # binary in target/release/rustflow
cargo test
./packaging/macos/build-app.sh # macOS: builds /Applications/rustflow.app
```

Layout: `crates/rustflow-proto` (wire protocol), `crates/rustflow` (app:
`clip/` clipboard backends, `input/` capture and injection, `files.rs` file
paste, `net.rs` transport, `tray/` menu bar and tray UIs).

## TODO

- [ ] Installers: signed and notarized `.dmg` for macOS, `.deb` for Ubuntu
- [ ] File paste Linux → Mac (lazy Finder paste)
- [ ] Encrypted connection (TLS with pairing)
- [ ] Image and rich-text clipboard
- [ ] Forward media keys (volume, brightness)
- [ ] Linux → Mac input, and more than two machines
- [ ] Auto Wake-on-LAN when the pointer is pushed toward a sleeping peer
- [ ] Proper multi-monitor layout on both sides
- [ ] Transfer progress for file paste (bytes, not just a spinner)
- [ ] Other Linux desktops (KDE Plasma, X11)
- [ ] App icon

## License

[MIT](LICENSE)
