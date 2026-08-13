# Wayland support matrix

Persea Desktop runs on Wayland, but several shell features that work on
X11 (and Windows/macOS) are limited or unavailable there. The
compositor owns the keyboard, the tray, the window positions and the
fullscreen transitions; a client can only ask. This document is the
consolidated limitation matrix: what works, what does not, and the
interim workarounds.

## Feature matrix

| Feature | X11 | Wayland | Notes / workaround |
|---------|-----|---------|--------------------|
| Global hotkeys (summon window, cycle sessions) | Works (global-shortcut plugin) | Unavailable | The plugin is X11-only and no-ops silently. The feature is switched off at startup and the settings page shows a note. Interim: bind the same actions in your compositor (custom shortcuts), see below. |
| Win/Super key into the session | Works (X11 hook) | Limited | XWayland passive grabs deliver the key while an XWayland window is focused (compositor-dependent); the focused-session inhibit protocol exists but is not wired. The toolbar Win button always works. See `wayland-keyboard.md`. |
| System tray | Works (AppIndicator/SNI) | Host-dependent | The tray speaks SNI (StatusNotifierItem). KDE Plasma shows it natively; GNOME needs the AppIndicator and KStatusNotifierItem extension; sway needs a tray-capable bar (swaybar includes one). No host = no tray. |
| Notifications | XDG daemon | XDG daemon | Same mechanism on both (D-Bus `org.freedesktop.Notifications`). Needs a running daemon (GNOME Shell, Plasma, or a standalone one like dunst). See `linux-troubleshooting.md`. |
| Kiosk mode | Best-effort | Best-effort | Soft kiosk on every platform. Wayland adds nothing worse: the window goes fullscreen as asked; the exit chord is shell-level and always works. See `kiosk.md`. |
| Tab strip docking | Works | Best-effort | The strip is positioned with `set_position`, which Wayland compositors may ignore. The strip can end up offset from the main window; it still hides in fullscreen/maximized/minimized states. |
| Multi-window sessions | Works | Works | Window creation, popping out, expand-to-monitor all work. |
| Drag-and-drop, clipboard | Works | Works | wry 0.47+ handles Wayland drag-drop; clipboard is native. |
| GPU / DMABUF | See troubleshooting | See troubleshooting | DMABUF renderer quirks (NVIDIA blank windows) are most visible on Wayland sessions. See `linux-troubleshooting.md`. |

## Global hotkeys: compositor-level interim

While the app's own global shortcuts are off on Wayland, the same
outcomes are reachable from the compositor's shortcut system. Bind
commands that focus or raise the window:

- GNOME (Mutter): Settings → Keyboard → View and Customize Shortcuts →
  Custom Shortcuts. A custom shortcut can run `persea-desktop` (focuses
  the existing instance via the single-instance behavior) or a
  window-focus tool.
- KDE Plasma: System Settings → Shortcuts → Custom Shortcuts.
- sway: a `bindsym` in the config with a `focus` command or
  `swaymsg`-driven window focus.

The settings page in the app keeps showing "global shortcuts
unavailable on Wayland" so the state is visible, not silent.

## Win/Super key: what ships

The X11 hook (`src-tauri/src/hooks/x11.rs`) also runs under Wayland
sessions, because compositors still export an X display for XWayland
clients. It arms passive grabs that several compositors (Mutter, KWin,
sway) deliver while the session window is focused. When the compositor
does not deliver, the session page's own toolbar Win button remains the
fallback. Full detail, including the `zwp_keyboard_shortcuts_inhibit_v1`
roadmap, lives in `wayland-keyboard.md`.

## Tray hosts

| Desktop | Tray visible | Requires |
|---------|-------------|----------|
| KDE Plasma | Yes | Nothing (native SNI) |
| GNOME | Yes | AppIndicator and KStatusNotifierItem extension |
| sway / wlroots | Yes | A bar with tray support (swaybar has it) |
| Weston / bare compositors | No | The app runs fine; tray features are just absent |

## Session type detection

The platform is detected once at startup from the environment
(`WAYLAND_DISPLAY` / `XDG_SESSION_TYPE`); a session type change while
the app runs is not picked up. Start the app from the same session you
want it to use.
