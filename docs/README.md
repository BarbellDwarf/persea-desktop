# Persea Desktop documentation

Persea Desktop is a desktop client for the persea remote access
server. Start with [getting-started.md](getting-started.md) if you are
new to the app.

## Install

- [install-windows.md](install-windows.md): installers, SmartScreen,
  uninstall, channels
- [install-macos.md](install-macos.md): dmg per architecture,
  Gatekeeper, uninstall, channels
- [install-linux.md](install-linux.md): deb, rpm (EPEL note),
  AppImage, NVIDIA workaround, tray and Wayland notes

## Using the app

- [getting-started.md](getting-started.md): install, add a server,
  log in, pair the device, open a session
- [transfers.md](transfers.md): drag files into sessions, the
  transfer window, downloads
- [hotkeys.md](hotkeys.md): global shortcuts, how to change them,
  Wayland note
- [kiosk.md](kiosk.md): locked-down fullscreen mode, the exit chord
- [keychain.md](keychain.md): where paired credentials are stored,
  Linux fallback stores
- [wayland.md](wayland.md): what works and what does not on Wayland
- [wayland-keyboard.md](wayland-keyboard.md): Win/Super key capture
  on Wayland
- [macos.md](macos.md): signing, Gatekeeper, fullscreen behavior
- [linux-troubleshooting.md](linux-troubleshooting.md): WebKitGTK,
  GStreamer and graphics quirks on Linux

## Enterprise

- [provisioning.md](provisioning.md): installer-injected server
  configuration, locked instances, kiosk pins, settings overrides

## Releases

- [release.md](release.md): how releases are built, per-OS artifacts
- [beta.md](beta.md): the beta channel, how to become a beta tester

## For developers

- [development.md](development.md): dev setup per OS, running and
  testing the app, the E2E suite, CI
- [bridge-events.md](bridge-events.md): the shell-to-page event
  contract (technical)
- `tests/e2e/README.md`: the end-to-end test harness (technical)

The app has no server inside it: point it at a persea instance you
run. Server documentation lives in the
[persea repository](https://github.com/persea-grove/persea).
