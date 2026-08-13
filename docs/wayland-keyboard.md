# Wayland keyboard capture spike (Super/Win key)

Status: **spike complete, limitation documented, best-effort shipped**.

The Win/Super key never reaches the webview on any platform. On Windows,
macOS and X11 the shell captures it with an OS hook and injects Meta_L
(0xFFE7) into the live session through the desktop bridge
(`key-inject { keysym, down }`, see `bridge-events.md`). Native Wayland
has no equivalent: the compositor owns the keyboard, and no client can
register a key that the compositor has not approved. This document is the
spike result: what works today, what does not, and the setup steps for
the fallback.

## What works

### 1. XWayland passive grabs (shipped, best-effort)

Compositors still export an X11 display for legacy clients (`DISPLAY` is
set in almost every session). The X11 hook (`src-tauri/src/hooks/x11.rs`)
connects to that display and arms `XGrabKey` passive grabs on the root
window while a session window is focused. Compositors that mediate
XWayland grabs deliver the grabbed key to the XWayland client when its
window is focused:

| Compositor | XWayland grab delivery |
|------------|------------------------|
| Mutter (GNOME) | Yes, while the XWayland window is focused |
| KWin (KDE) | Yes, while the XWayland window is focused |
| sway / wlroots | Yes, while the XWayland window is focused |
| Weston | No dedicated key routing |

Caveat: the same limitation as X11 applies, a client that already owns
the bare Super combo (GNOME Shell's own binding on X11) wins, and the
grab is silently absent. The toolbar Win button remains the fallback.

### 2. `zwp_keyboard_shortcuts_inhibit_v1` (focused-session inhibit)

The compositor-side mechanism for exactly this case: while the client's
surface is focused and holds the inhibit, compositor shortcuts are
suppressed and the keys are delivered to the client as ordinary key
events. Compositor support:

- Mutter: 49+ (GNOME 47+)
- KWin: 6.6+ (Plasma 6.6+)
- sway: 1.11+

For persea this is a **focused** capture, not a global one: the webview
would need to bind the inhibit on its GTK surface. Tauri 2.11 exposes no
GTK surface handle to the app, so this path is documented, not wired.
A future implementation would use `wayland-client` +
`zwp_keyboard_shortcuts_inhibit_v1` against the webview's surface (via a
GTK-side patch or a Wayland protocol extension).

### 3. evdev / uinput (setup-step fallback)

A uinput device can capture physical keys and re-emit them, which is the
classic global interception route. Requirements:

1. The user (or the setup flow) creates an input group and adds the
   account to it: `sudo groupadd input; sudo usermod -aG input $USER`
   (re-login required).
2. The user grants `uinput` access for the group via a udev rule:

   ```
   KERNEL=="uinput", GROUP="input", MODE="0660"
   ```

3. The app captures Super via evdev and injects Meta_L through the
   bridge, optionally re-emitting the key with uinput when the session is
   not focused (re-synthesis; needs the input group at minimum).

This is a global interception tool with privilege and security
implications (the app sees every key). It belongs in the setup flow as
an explicit opt-in, not silently in the app. Not shipped.

## What does not work

- **Global key grab on native Wayland**: no client API exists, by
  design. `org.freedesktop.portal.GlobalShortcuts` only registers
  compositor-approved shortcuts and does not deliver key state changes.
- **`org.freedesktop.portal.RemoteDesktop` today**: the portal can
  deliver keys to a session, but compositor support for keyboard
  delivery without a full remote session is still uneven (as of the
  spike); this is the long-term path.

## Decision

No broken global hook ships. The app attempts, in order:

1. X11 (covers X11 sessions and the XWayland best-effort on Wayland);
2. on failure with a native Wayland session, logs the limitation note
   (`wayland::LIMITATION_NOTE`) and disables capture.

Injection still requires a focused session window
(`hooks::set_session_focus`) and the desktop bridge
(`bridge::desktop_bridge_available`); a page without the bridge keeps
the toolbar Win button working unchanged.

## Verdict

Capture works on Windows, macOS and X11 today; on Wayland the shipped
state is the XWayland focused best-effort, with shortcuts-inhibit as the
documented follow-up and RemoteDesktop as the long-term goal.
