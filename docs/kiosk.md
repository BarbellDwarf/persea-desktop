# Kiosk mode

Kiosk mode turns the shell into a locked-down thin-client terminal: the app
opens fullscreen on the instance's connections page and the user gets session
UI and nothing else. This is a soft kiosk: the app locks its own surface, it
cannot perform OS-level lockout.

## Scope

| Allowed | Blocked |
|---------|---------|
| Connections page | Server admin pages (`/admin`, `/admin/*`, `/admin.html`, `/admin/users.html`, ...) |
| Session pages (inline tabs, popped session windows) | Server setup wizard (`/setup`) |
| Transfer window | Account pages (`/account`, `/account/*`, `/account/tokens.html`, ...) |
| Session-ended notifications | Shell settings and pairing pages (unreachable: the viewport stays on the instance) |
| | Tray (never created while kiosk is active; removed when kiosk is entered mid-session, restored on exit) |
| | Global hotkeys (all except the exit chord) |
| | Window closing, resizing, maximization, devtools |

The blocklist is enforced at the URL path level in the webview navigation
handlers (the handler consult is wired in v1.1.0); outside kiosk mode the
same paths behave as usual.

## Activation

Precedence: **provision override > per-instance user config > off**.

- The provision document (`provision.json` / HKLM / machine file, see
  `docs/provisioning.md`) can pin kiosk on or off with `"kiosk": { "enabled":
  true }`. A pin is locked: the user setting cannot flip it, and a pinned-on
  kiosk re-enters on every launch.
- Without a pin, the per-instance setting `kioskAllowed` in `instances.json`
  decides.
- The decision applies to the startup instance (default, else first
  configured). Kiosk deployments set a provisioned default, which always
  wins.

Two gates apply on top, both fail closed:

1. **Server gate**: the instance probe must advertise `kiosk_allowed`. No
   probe, no capability: kiosk is unavailable, the config is ignored, the
   exit chord is inert, and the shell never shows a kiosk toggle for that
   server (the Settings kiosk toggle ships in v1.1.0 and appears only for
   servers that pass this gate).
2. **Escape-hatch gate**: the exit chord must actually register (see below).
   A conflicted chord or an unsupported platform keeps kiosk off with a
   warning. A kiosk without an exit is a trap.

## Entering and leaving mid-session

Kiosk is not only a startup state. Two live controls drive it, both
server-gated on the probe's `kiosk_allowed` capability:

- **Tray toggle**: each instance submenu carries a "Kiosk mode" check item
  when the server supports kiosk. Clicking it emits the `kiosk-toggle`
  event with `enabled: true`; the listener enters kiosk for that instance.
- **Settings toggle**: the Settings → Kiosk section lists one toggle per
  kiosk-capable server. It emits the same `kiosk-toggle` event, so both
  controls share one listener.

Entry runs the same gates as startup (provision pin off, missing
capability, or a chord that cannot register all refuse entry) and lands
the viewport on the target instance's connections page, so a toggle from
the settings page leaves the shell page behind. The exit chord is
re-registered on entry: exiting releases it, and re-entering without
re-registering would leave the kiosk with no way out.

While kiosk is active the tray icon is removed: the menu click that
started kiosk fires its event first, so removing the icon from inside the
event is safe. Exiting restores the tray. On Wayland the tray usually
never existed, and the settings toggle shows the refusal reason in the
section note; the tray toggle just logs.

## Window behavior

On entry the main window becomes fullscreen, undecorated, non-resizable and
non-maximizable; the tab strip hides; the global hotkeys disable; close
requests are blocked. `set_decorations` and `set_resizable` are runtime APIs
in tauri 2.11, so exit restores the window exactly. Devtools are builder-only
and compiled out of release builds; the dispatcher applies `.devtools(false)`
to the main window builder in kiosk mode to close the debug-build gap.

Session pages open inline in the kiosk window (the tab strip is hidden, so
tabs are not switchable in kiosk; a new session simply replaces the
viewport). Popped session windows behave normally.

## Exit: the secret chord

`Ctrl+Alt+Shift+Q` is the only way out. The chord is a global shortcut
registered only while kiosk is active; it fires at the OS level, so it works
when the webview is frozen, dead or showing an error page.

**Confirmation: press the chord twice within 3 seconds.** The first press
arms the exit, the second one inside the window confirms it; a single stray
press changes nothing. Confirming restores the window (windowed, decorated,
resizable), the tab strip and the hotkeys, and returns the app to normal
mode.

Why a second press instead of a native dialog: tauri 2.11.5 exposes no
window-level key event hook (verified: no `on_key_event` anywhere in the
tauri / tauri-runtime / tauri-runtime-wry sources), and the dialog
plugin's confirm round trip is not wired in this tree: the plugin itself
is registered (it backs the download save dialog), but the confirm
script only activates when the server page emits a request event. The
confirm step is one function
(`kiosk::on_chord_press`), so a dialog-based confirm can replace the
second press once the round trip is wired.

In a pinned (provisioned) kiosk, the chord still exits for the session; the
next launch re-enters kiosk. There is no other way out of kiosk mode while it
is active: the tray is absent (removed on mid-session entry, never created
at startup), hotkeys are off, close is blocked. Exiting restores the tray
icon when one existed before entry.

## Wayland limitations

The global-shortcut plugin is X11-only and silently no-ops on Wayland, so
the exit chord cannot be registered there. The escape-hatch gate therefore
refuses kiosk on Wayland entirely: the app runs normally and the kiosk
setting is ignored, with a warning. Do not deploy kiosk on Wayland
compositors; X11, Windows and macOS are supported.

A chord conflict (another program already owns `Ctrl+Alt+Shift+Q`) has the
same effect on supported platforms: kiosk stays off and the conflict is
logged.

## Unreachable instance at boot

Kiosk enters from the cached probe: an instance that was reachable when last
checked enters kiosk even if it is unreachable at boot. The web error page
renders inside the kiosk window, and the exit chord still works (it is
shell-level). There are no kiosk-specific crash paths: every side effect is
best-effort. A never-probed instance fails the server gate and simply runs
normally.

## Dispatcher wiring

The kiosk module registers its own `kiosk-toggle` listener (in `setup`),
so the toggles need no dispatcher wiring. The startup wiring the
dispatcher must keep:

1. Declare `mod kiosk;` in `src-tauri/src/lib.rs`.
2. In the setup hook, after `instances::setup` and `hotkeys::setup`, before
   the main window is built: `kiosk::setup(app)?;`. This registers the
   `kiosk-toggle` listener, resolves the startup decision and registers
   the exit chord when kiosk is wanted.
3. On the main window builder: `.devtools(false)` when `kiosk::active()`.
4. After `windows::setup` (the tab strip must exist for it to be hideable):
   `if kiosk::active() { kiosk::enter(app.handle()); }`.
5. In the windows.rs navigation handlers, in the `Decision::Allow` arm of
   both `navigation_handler_for` and `viewport_new_window_handler`, consult
   the kiosk blocklist and block when it hits:

   ```rust
   if crate::kiosk::navigation_blocked(url) {
       log_blocked(url);
       return false; // or NewWindowResponse::Deny in the new-window handler
   }
   ```

   No navigation-policy rebuild is needed: the consult reads live state, so
   kiosk entry and exit take effect immediately.
6. The tray feature must not create the tray while `kiosk::is_active()`.
   Mid-session entry and exit are handled inside the kiosk module:
   `kiosk::enter`/`exit` drive `tray::set_kiosk`, which removes the tray
   icon on entry and recreates it on exit. Dispatcher work: none beyond
   the existing calls.

No Cargo.toml or capability changes are needed by kiosk itself.

## Verification matrix

| Check | How |
|-------|-----|
| Kiosk launch | Enable kiosk, launch: fullscreen, undecorated, non-resizable, lands on the connections page |
| Nav lockdown | In kiosk (v1.1.0): `/admin`, `/admin.html`, `/admin/users.html`, `/setup`, `/account/tokens.html` blocked; `/` and `/client/<id>` reachable. Outside kiosk: the same paths behave as usual |
| Exit chord | `Ctrl+Alt+Shift+Q` twice within 3 s: kiosk exits, window/hotkeys/strip restore. Single press: nothing |
| Close blocked | Alt+F4 / window close during kiosk: nothing happens |
| Frozen page | Freeze the webview (kill the instance, load an error page): the chord still exits |
| Hotkeys off | Summon / cycle-sessions chords do nothing in kiosk; they work again after exit |
| Server gate | Server without `kiosk_allowed`: kiosk setting ignored, no toggle, chord inert |
| Provision pin | `kiosk.enabled: true` overrides a user-off setting; `false` overrides a user-on one (and refuses mid-session toggles) |
| Chord conflict | Another program owns the chord: kiosk stays off with a warning |
| Wayland | Kiosk refused, app runs normally, limitation logged; the settings toggle shows the refusal reason |
| Tray toggle | Instance submenu "Kiosk mode" enters kiosk for that instance; the tray icon disappears; the chord exits and the tray returns |
| Settings toggle | Settings → Kiosk: one toggle per kiosk-capable server; clicking enters kiosk for that server and leaves the shell page |
