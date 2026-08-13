# Global hotkeys

App-level shortcuts that work while Persea Desktop runs, foreground or
background. This document is the contract for the shell settings page,
the session window manager and the kiosk feature.

## Defaults

| Action | Default chord | What it does |
|--------|---------------|--------------|
| Summon window | `Ctrl+Alt+P` | Shows, unminimizes and focuses the main window. |
| Cycle sessions | `Ctrl+Shift+Tab` | Emits `hotkey-cycle-sessions` on the main window; the session window manager advances to the next open session window. |

Both chords are user-configurable in Settings → Shortcuts and persist
in `hotkeys.json` in the per-user config directory (same pattern as
`shell.json`). Chord syntax is the global-hotkey grammar: one or more
modifiers (`ctrl`, `alt`, `shift`, `super`) followed by a key (`p`,
`tab`, `f5`, `space`, `arrowup`, …), joined with `+`, e.g. `ctrl+alt+p`.

## Platform support

- Windows, macOS, Linux/X11: chords register with the OS and fire while
  the app is backgrounded.
- Linux/Wayland: global key grabbing is not available; the plugin is
  X11-only and silently no-ops there. The settings page shows "global
  shortcuts unavailable on Wayland", the feature stays off, and the app
  remains fully functional. Interim workaround: bind the same actions
  in your compositor (window focus for summon, window cycling for
  sessions).
- The platform is detected once at startup from the session environment
  (`WAYLAND_DISPLAY` / `XDG_SESSION_TYPE`); the session type cannot
  change while the app runs.

## Conflicts

A registration that fails (the OS or another program owns the chord, or
the chord is already used by the other shortcut) logs a warning and
marks the shortcut as "Conflict" in the settings page. The chord stays
inactive until the user picks a different one; there is no auto-fallback.
Saving a conflicted chord again retries the registration, so a chord
that was freed by the other program in the meantime activates.

## Kiosk mode

In kiosk mode every shortcut is suppressed at runtime: the kiosk
feature calls the module's enable/disable entrypoint, which unregisters
all chords and marks them "Disabled" in the settings page. Re-enabling
restores the registered chords.

## Events

- `hotkey-cycle-sessions` — emitted on the main window (Tauri event, no
  payload) when the cycle-sessions chord fires. The session window
  manager listens for it and switches to the next session window.
  Remote pages that happen to hear it gain nothing: the payload is
  empty and the event only signals the manager action.
