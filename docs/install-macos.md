# Installing on macOS

Persea Desktop ships as a `.dmg` disk image, one per architecture:

- `Persea-Desktop-<version>-aarch64.dmg` for Apple Silicon (M-series).
- `Persea-Desktop-<version>-x86_64.dmg` for Intel Macs.

The app supports macOS 10.15 and newer.

## Download

Get the dmg for your Mac's architecture from the
[Releases page](https://github.com/persea-grove/persea-desktop/releases)
on GitHub. Not sure which one you need? Check Apple menu → About This
Mac: Apple Silicon or Intel tells you which dmg to download.

## Install

1. Open the downloaded dmg and drag **Persea Desktop** into your
   Applications folder.
2. Launch it from Applications.

## Gatekeeper: the first-launch warning

The app is ad-hoc signed: it has no Apple Developer certificate, so
macOS marks it as an unidentified developer. The first launch shows
**"Persea Desktop" cannot be opened because the developer cannot be
verified.**

Two ways past the warning:

1. Right-click (or Ctrl+click) the app in Finder → **Open** → **Open**
   in the dialog that appears. This records an exception for that
   binary.
2. System Settings → Privacy & Security, scroll to the security
   section, click **Open Anyway** next to the Persea Desktop entry.

**macOS 15 (Sequoia) is stricter** about the bypass paths. If the
right-click route does not work on your macOS version, the Open Anyway
entry in Privacy & Security is the more reliable one.

There is no paid workaround on your side: notarization is planned, and
until it ships every rebuilt build behaves like a fresh unknown app.

## Every update re-prompts

Each update replaces the app bundle, which carries a fresh ad-hoc
signature and a fresh quarantine state. Expect the Gatekeeper warning
again after every update until the app is notarized. That is expected,
not a bug.

## Uninstall

Quit the app and drag **Persea Desktop** from Applications to the
Trash. Paired device tokens live in the macOS Keychain and are not
removed by uninstalling; revoke a device from Settings → Device
pairing (v1.1.0), or from the tray's server menu → **Pair this
device…**, if you want it gone.

## Updates and channels

The app checks for updates automatically (on startup and every 4
hours). In v1.1.0, Settings → Updates adds the manual **Check for
updates** and **Download & restart** actions. To update manually,
download the new dmg and replace the app in Applications (drag the
new one over the old).

Two channels exist:

- **Stable**: the Releases page. Version numbers like `1.0.0`.
- **Beta**: the beta pre-release, versioned `1.0.0-beta.<run number>`,
  downloadable from the same page. The beta release is rebuilt
  frequently and its download links are only valid between builds.

Installing a beta moves you to the beta channel: beta installs update
from the beta channel through the updater. Leaving the beta channel
means installing the stable installer, which updates from stable from
then on. See [beta.md](beta.md).

## macOS-specific notes

- **Fullscreen.** The View menu's Fullscreen item and the tab
  strip's expand-to-monitor action use the native macOS fullscreen.
  Element fullscreen inside a page (the web client's own fullscreen
  button, video playback) is not supported in this version: use the
  shell fullscreen instead.
- **Notifications.** First enabling notifications in Settings →
  Notifications triggers the macOS permission prompt. Notifications
  are posted from the app; if you denied permission once, re-enable it
  in System Settings → Notifications.
