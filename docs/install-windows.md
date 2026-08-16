# Installing on Windows

Persea Desktop ships as two Windows installers, both produced from the
same build:

- **NSIS**: `Persea-Desktop-<version>-setup.exe`, the recommended
  installer.
- **MSI**: `Persea-Desktop-<version>-amd64.msi`, for managed
  deployments (Group Policy, software management tools).

## Download

Get the installer from the [Releases page](https://github.com/persea-grove/persea-desktop/releases)
on GitHub. Stable releases publish both installers; the beta
pre-release publishes the same matrix for the beta channel.

## Install

1. Run the installer. Windows 10 and 11 include the WebView2 runtime
   the app needs; if it is missing, the installer downloads it
   silently during setup.
2. **SmartScreen warning.** The installer is not yet code-signed, so
   SmartScreen shows "Windows protected your PC". Click **More info**,
   then **Run anyway**. This is expected for every build until an EV
   code-signing certificate is in place.
3. Follow the installer to completion. The app installs per-user; you
   do not need administrator rights.

## First launch

The app opens on the welcome page. Add your server and log in; the
steps are in [getting-started.md](getting-started.md).

## Uninstall

- Settings → Apps → Installed apps → Persea Desktop → Uninstall, or
- Control Panel → Programs and Features → Persea Desktop → Uninstall.

Uninstalling removes the app but keeps nothing else: no server data
lives on the machine. Paired device tokens live in the Windows
Credential Manager; uninstalling the app does not remove them. Revoke
a device from Settings → Device pairing (v1.1.0), or from the tray's
server menu → **Pair this device…**, first if you want it gone.

## Updates and channels

The app checks for updates automatically (on startup and every 4
hours). In v1.1.0, Settings → Updates adds the manual **Check for
updates** and **Download & restart** actions. To update manually,
install the newer installer over the current installation; your
servers and pairing survive.

Two channels exist:

- **Stable**: the Releases page. Version numbers like `1.0.0`.
- **Beta**: the beta pre-release, versioned `1.0.0-beta.<run number>`.
  Beta installers come from the same download page. The beta release
  is rebuilt frequently and its download links are only valid between
  builds.

Installing a beta moves you to the beta channel: beta installs update
from the beta channel through the updater. Leaving the beta channel
means installing the stable installer, which updates from stable from
then on. See [beta.md](beta.md).

## Windows-specific notes

- **Hotkey conflicts.** Windows reserves some chords for itself, for
  example `Win+L` and `Ctrl+Alt+Del`. If a shortcut in Settings →
  Shortcuts shows as a conflict, pick a different chord.
- **Notifications.** Toasts are delivered through Windows
  notifications. If you run a portable build rather than an installed
  one, toasts may be silently dropped: the installer registers the app
  for notifications, a bare binary does not.
- **WebView2.** The app runs on the Evergreen WebView2 runtime, which
  is preinstalled on Windows 11 and broadly deployed on Windows 10.
  The installer bootstraps it when it is missing.
