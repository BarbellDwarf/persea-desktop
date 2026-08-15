# Installing on Linux

Persea Desktop ships three Linux formats per release:

- **`.deb`** for Debian 13 (trixie) and derivatives, including Ubuntu.
- **`.rpm`** for RHEL 10 and derivatives (AlmaLinux, Rocky), and
  Fedora.
- **`.AppImage`** for any other distribution.

The Linux build runs on WebKitGTK 4.1, the web engine Tauri 2
requires. It is not Chromium: graphics, codecs and notifications
behave differently, and the quirks are collected in
[linux-troubleshooting.md](linux-troubleshooting.md).

## Download

Get the package for your distribution from the
[Releases page](https://github.com/persea-grove/persea-desktop/releases)
on GitHub. Stable releases publish the full matrix; the beta
pre-release publishes the same matrix for the beta channel.

## Debian / Ubuntu (.deb)

```sh
sudo apt install ./persea-desktop_<version>_amd64.deb
```

The package declares its dependencies explicitly, so apt pulls in the
WebKitGTK 4.1 engine plus the GStreamer codec stack (H.264 decode,
VA-API hardware decode) automatically.

## RHEL 10 / derivatives (.rpm)

RHEL 10 removed WebKitGTK from the base repositories. Enable EPEL 10
first, then install:

```sh
sudo dnf install epel-release
sudo dnf install ./persea-desktop-<version>.rpm
```

The rpm declares `webkit2gtk4.1` as a dependency, so the missing EPEL
repo shows up immediately as an unsatisfied dependency. Fedora ships
webkit2gtk4.1 in its default repositories; the rpm installs there
without EPEL.

## Other distributions (.AppImage)

```sh
chmod +x Persea-Desktop-<version>.AppImage
./Persea-Desktop-<version>.AppImage
```

An AppImage carries the app itself but not the web engine. Install the
WebKitGTK 4.1 stack by hand first:

```sh
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1 \
  gstreamer1.0-libav gstreamer1.0-plugins-bad gstreamer1.0-vaapi mesa-va-drivers

# Fedora / RHEL + EPEL
sudo dnf install webkit2gtk4.1 libappindicator-gtk3 \
  gstreamer1-libav gstreamer1-plugins-bad gstreamer1-vaapi mesa-dri-drivers
```

## First launch

The app opens on the welcome page. Add your server and log in; the
steps are in [getting-started.md](getting-started.md).

## Uninstall

```sh
# Debian / Ubuntu
sudo apt remove persea-desktop

# Fedora / RHEL
sudo dnf remove persea-desktop

# AppImage
rm ~/Applications/Persea-Desktop-*.AppImage
```

Paired device tokens live in your desktop keyring (or a fallback
store, see [keychain.md](keychain.md)) and are not removed by
uninstalling. Revoke a device from Settings → Device pairing
(v1.1.0), or from the tray's server menu → **Pair this device…**, if
you want it gone.

## Updates and channels

The app checks for updates automatically (on startup and every 4
hours). In v1.1.0, Settings → Updates adds the manual **Check for
updates** and **Download & restart** actions. To update manually,
install the newer package over the current one; your servers and
pairing survive.

Two channels exist:

- **Stable**: the Releases page. Version numbers like `1.0.0`.
- **Beta**: the beta pre-release, versioned `1.0.0-beta.<run number>`,
  downloadable from the same page. The beta release is rebuilt
  frequently and its download links are only valid between builds.

Installing a beta moves you to the beta channel: beta installs update
from the beta channel through the updater. Leaving the beta channel
means installing the stable package, which updates from stable from
then on. See [beta.md](beta.md).

## Linux-specific notes

- **NVIDIA blank window.** On some NVIDIA drivers the window opens
  blank (window chrome only, no page content). Start the app with
  `WEBKIT_DISABLE_DMABUF_RENDERER=1 persea-desktop`, or add
  `WEBKIT_DISABLE_COMPOSITING_MODE=1` too if that alone does not fix
  it. For desktop files, put `env WEBKIT_DISABLE_DMABUF_RENDERER=1`
  in front of the `Exec=` line. Details in
  [linux-troubleshooting.md](linux-troubleshooting.md).
- **Tray.** The tray icon speaks the StatusNotifierItem protocol. KDE
  Plasma shows it natively; GNOME needs the AppIndicator and
  KStatusNotifierItem extension; sway needs a tray-capable bar. No
  tray host means no tray; the app runs fine without it.
- **Notifications.** Notifications go through the XDG notification
  daemon (D-Bus). GNOME Shell, KDE Plasma and standalone daemons
  (dunst, mako) work; a desktop without any daemon silently drops
  them.
- **Wayland.** The app runs on Wayland, but some features are limited
  or unavailable there: global hotkeys do not work (bind them in your
  compositor instead), the Win/Super key into sessions is
  best-effort, kiosk mode is refused, and the tab strip docking is
  best-effort. The full matrix is in [wayland.md](wayland.md).
