# Linux troubleshooting: WebKitGTK, GStreamer, graphics

The Linux build of Persea Desktop runs on WebKitGTK 4.1 (GTK3 +
libsoup3), the engine Tauri 2 requires. WebKitGTK is not Chromium:
graphics, codecs, autoplay and notifications all behave differently.
This page collects the known quirks and the verification procedures.

## Engine versions per distro

| Distro | Package | WebKitGTK |
|--------|---------|-----------|
| Debian 13 (trixie) | `libwebkit2gtk-4.1-0` | 2.50.x |
| Ubuntu 22.04 / 24.04 | `libwebkit2gtk-4.1-0` | 2.50.x |
| RHEL 10 (with EPEL 10) | `webkit2gtk4.1` | 2.48+ / 2.50 |

RHEL 10 removed WebKitGTK from the base repos. Users must enable EPEL 10
before installing the rpm (`dnf install epel-release`, then the app);
the rpm declares `Requires: webkit2gtk4.1` so the dependency check makes
the missing repo obvious.

## NVIDIA blank-window workaround

On some NVIDIA drivers the WebKitGTK compositor renders a blank window
(no page content, window chrome only). The WebKit workarounds are two
environment variables, used alone or together:

```
WEBKIT_DISABLE_DMABUF_RENDERER=1
WEBKIT_DISABLE_COMPOSITING_MODE=1
```

- `WEBKIT_DISABLE_DMABUF_RENDERER=1` fixes the NVIDIA/DMABUF cases and
  is the first thing to try.
- `WEBKIT_DISABLE_COMPOSITING_MODE=1` disables accelerated compositing
  entirely (CPU rendering). It fixes the widest set of drivers but
  costs performance on canvas-heavy sessions.

Apply them to the launch environment. For a manual run:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 persea-desktop
```

For desktop files, add `env WEBKIT_DISABLE_DMABUF_RENDERER=1` to the
`Exec=` line. They are process-startup settings: WebKit reads them
before the first webview exists, so exporting them after launch does
nothing. The settings page will gain a "Hardware acceleration" toggle
that applies these variables automatically (currently the variables are
documented-only, never set by default).

## GStreamer codecs and H.264

The web client renders the remote display on a canvas and decodes RDP
H.264 with WebCodecs where the engine supports it; audio playback and
any engine-mediated decode go through GStreamer. The Debian package
declares the codec stack as hard dependencies, so `apt install` on
Debian/Ubuntu pulls everything:

- `gstreamer1.0-libav`: FFmpeg-based decode (software fallback)
- `gstreamer1.0-plugins-bad`: H.264/AAC elements
- `gstreamer1.0-vaapi`: VA-API hardware decode integration
- `mesa-va-drivers`: the VA-API driver (AMD/Intel; NVIDIA uses
  nvidia's own driver)

Users of the rpm or AppImage install the same stack by hand:

```sh
# Fedora / RHEL + EPEL
sudo dnf install gstreamer1-libav gstreamer1-plugins-bad gstreamer1-vaapi mesa-dri-drivers
```

Check what is actually loaded with:

```sh
gst-inspect-1.0 avdec_h264   # libav software decoder
gst-inspect-1.0 vaapidecode  # VA-API hardware decoder
```

### RDP H.264: verify decode or confirm the fallback

RDP sessions default to the H.264 pipeline on the server (it is a
per-connection setting). WebKitGTK's WebCodecs support is partial, so
the decode path on Linux must be verified per engine version:

1. Open an RDP session on Linux in the desktop app, on a desktop with
   moving content (a video or a scrolling window on the remote).
2. **H.264 path**: the display stays smooth and sharp. No further
   action.
3. **JPEG fallback**: the client detects it cannot decode and switches
   to JPEG tiles. The display still updates (visible tile refresh
   during motion), there is no black screen and no error loop. This is
   the acceptable fallback: note the `webkit2gtk` version
   (`apt-cache policy libwebkit2gtk-4.1-0` / `rpm -q webkit2gtk4.1`) in
   the issue report.
4. **Neither**: black screen or a repeating decode error. Turn H.264
   off for the connection in the server's per-entry settings (the
   toggle the server exposes per connection) and re-test. Keep it off
   on that server until the shipped WebKitGTK version decodes H.264.

The result should be recorded per distro/engine version; the JPEG
fallback existing at all is the pass condition for 1.2.0 on Linux.

## RDP audio and autoplay

Autoplay policy is per engine. Chromium-based WebView2 allows
autoplay by default; WebKitGTK is stricter and may hold audio until
the page receives a user gesture. Verification procedure:

1. Open an RDP session on Linux, play audio on the remote desktop.
2. If no sound: click inside the session display once (a real gesture
   inside the page) and replay. A gesture normally unlocks audio.
3. If audio still does not start, check the volume policy of the
   desktop environment (some minimal setups route web audio to the
   wrong sink; `pavucontrol` shows the app's stream).

## Notifications

Notifications go over the XDG D-Bus daemon
(`org.freedesktop.Notifications`); the app talks to it directly, it
does not use the Tauri notification plugin, because that plugin hits a
GNOME 46+ bug where notifications close immediately or never render.
What that means in practice:

- GNOME Shell, KDE Plasma and any standalone daemon (dunst, mako)
  work.
- A desktop without any daemon silently drops notifications. There is
  no error; enable a daemon if you expect toasts.
- Test: enable notifications in Settings (enabling sends a test
  notification), then end a session and watch for the notification.

## Theme follows GTK, not the OS

WebKitGTK reports the GTK theme's dark/light preference via
`prefers-color-scheme`, not the OS-level setting. If the remote UI's
dark mode does not match the system setting, change the GTK theme
(GNOME: Settings → Appearance; or `GTK_THEME=Adwaita:dark` in the
launch environment). The shell's own pages read the app's theme
setting, which is independent of this.
