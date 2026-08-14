# Getting started

Persea Desktop is a desktop client for a persea server. The app does not
include a server: you point it at a persea instance you already run (the
web app at `https://your-server`), log in there, and the app hosts the
remote desktop sessions in its own windows.

This guide walks you from a fresh install to your first session.

## What you need

- A persea server. Version 1.2.0 or newer unlocks the full desktop
  feature set (device pairing, session events, the capability probe).
  The drive API used by file transfers exists from server 1.1.1. The
  About section in the app's Settings shows the minimum versions.
- The Persea Desktop app, installed for your operating system:

  | OS | Install guide |
  |----|---------------|
  | Windows | [install-windows.md](install-windows.md) |
  | macOS | [install-macos.md](install-macos.md) |
  | Linux | [install-linux.md](install-linux.md) |

## 1. Install the app

Follow the install guide for your OS. The app is a normal desktop
program: an installer on Windows, a `.dmg` on macOS, a package or
AppImage on Linux. First-launch warnings (SmartScreen on Windows,
Gatekeeper on macOS) are expected because the installers are not yet
code-signed; the install guides explain how to get past them.

## 2. Add your server

Launch Persea Desktop. The first window shows the welcome page with an
"Add your first server" form:

1. Give the server a name, for example "Production".
2. Enter its URL, for example `https://persea.example.com`. Plain
   `http` is accepted only for localhost.
3. Click **Add server**. The app probes the server and shows its
   version and capabilities.
4. Click **Open server** to open it.

You can add more servers any time in Settings → Instances. Each server
keeps its own login, sessions and data store, so signing in to one
never touches another. The app opens the default (or last used)
instance automatically at startup.

## 3. Log in

The app opens the server in its own webview window. Log in with any
method the server supports: password, SSO (OIDC or SAML), MFA, all
behave exactly as in a browser.

Two things to know:

- The webview is locked down to your configured servers and the
  identity providers their login redirects to. Links to other sites
  open in your system browser instead of inside the app.
- The server's login lives in the app's per-instance webview store. It
  survives app restarts, and it is separate per server.

## 4. Pair this device

Pairing registers this device with the server. The paired token is
stored in your OS keychain and powers the tray's session list,
notifications, drag-drop file transfers and signed-out detection.

Pairing is offered only when the server advertises it (it is an admin
toggle on the server). If the pairing page says device pairing is
disabled, ask the server administrator.

To pair:

1. Open Settings → Device pairing (or right-click the tray icon, open
   the server's menu and choose **Pair this device…**).
2. Click **Pair this device**. The app shows an 8-character code.
3. Click **Open pairing page**. The app navigates to the server's
   account tokens page, where you are already logged in.
4. Paste the code there and confirm the device.
5. The app polls for approval and stores the token when the server
   approves it.

The code expires after 10 minutes. You can cancel at any time.

The token belongs to the user who confirms the code. If you use several
accounts on the same server (for example a day-to-day account and an
admin account), pair once per account: each pairing gets its own token
and each can be revoked separately. Re-pairing from the same account
replaces that account's token.

Revoke a device from Settings → Device pairing, or from the server's
account tokens page.

## 5. Connect to a session

Open a session from the server's connections page as usual. Instead of
a browser tab, the session opens in the app:

- **Tabs.** Each session is a tab in a strip docked to the top of the
  main window. Switch tabs, close them, and use the tab's menu to pop
  a session out into its own window or expand it to fullscreen on a
  monitor.
- **Ended sessions.** When a session ends, its tab shows a Reconnect
  button for a few seconds, then closes. Closing a tab never ends the
  server-side session: sessions stay alive on the server, exactly like
  closing a browser tab. Terminate sessions from the server's sessions
  page.
- **Menu bar.** File → New Session opens the server's sessions page,
  Close Tab closes the active tab, View toggles fullscreen and the tab
  strip.

## What you can do next

- **File transfers**: drag files from your file manager onto an RDP
  session window to upload them to the session's drive. See
  [transfers.md](transfers.md).
- **Notifications**: session started, ended, error and idle-warning
  alerts. They are off by default; enable them in Settings →
  Notifications (the first enable sends a test notification).
- **Global hotkeys**: `Ctrl+Alt+P` summons the window, `Ctrl+Shift+Tab`
  cycles sessions. Change them in Settings → Shortcuts. See
  [hotkeys.md](hotkeys.md).
- **The tray**: the tray menu lists your servers and their live
  sessions, shows pairing status and offers About and Quit. The tray
  icon shows a dot while sessions are active and a hollow ring when a
  server rejected your token (re-pair to resume).
- **Kiosk mode**: if your administrator enabled it, the app can lock
  itself to a single server in fullscreen. See [kiosk.md](kiosk.md).
