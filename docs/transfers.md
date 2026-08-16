# File transfers

Persea Desktop moves files between the local machine and remote sessions
by dragging them from the OS onto a session window. Uploads stream
straight into the session's RDP drive over the server's drive REST API;
downloads from the drive browser are intercepted shell-side and land in
a folder you control.

## Sending files to a session (drag-drop upload)

1. Open an RDP session with file transfer enabled (the session's drive).
2. Drag one or more files from your file manager onto the session
   window. A drop-zone overlay follows the cursor while you drag.
3. Drop. The files upload to the drive and the transfer window opens,
   showing per-file progress (reading, then uploading, then done).
4. Files with the same name as an existing drive file trigger a native
   prompt: **Overwrite**, **Rename** (an automatic `name (1).ext`
   suffix), or **Cancel**.

Requirements:

- The server must advertise the `desktop_transfers` capability (admin
  toggle). When it is off, drops are ignored with a "transfers disabled
  by this server" notice; the web UI's in-session upload button still
  works.
- The device must be paired (device pairing from Settings → Device
  pairing in v1.1.0, or from the tray's server menu). The paired
  token is the upload identity: only the owner of the session (or an
  admin) can upload to it.
- Files up to 1 GiB are accepted by the shell path (the file is
  buffered while uploading). Larger files: use the upload button inside
  the session, which streams through guacd without the shell cap.
- LUKS-encrypted drives work: the server decrypts the drive before the
  upload lands.

Limitations (v1.0.0):

- **SSH sessions**: drag-drop is not supported (the SSH drive has no
  REST upload path). Dropping on an SSH session shows a notice pointing
  at the in-session upload button.
- A drop without paths (a known KDE Wayland quirk) shows a notice
  instead of silently doing nothing; retry the drag.
- On Wayland, the drop-zone overlay may not track the cursor (the
  compositor owns window positions); the drop itself still works.

## Receiving files (downloads)

Two paths exist:

- **In the session's drive browser**: clicking download in the web
  client triggers the webview's download interception, which saves the
  file into the OS Downloads folder (collision-free names). It never
  lands in an invisible webview default directory.
- **Shell-side "Save as"**: download rows in the transfer window whose
  source is the drive REST API offer **Save as**, which re-downloads
  the file with the paired device token through a native save dialog,
  so you pick the exact location and name.

Every download (from either path) appears in the transfer window with
an **Open folder** action. **Clear finished** removes done, failed and
cancelled rows.

## The transfer window

A small shell window listing transfers: direction (up/down), file name,
status, progress, error text for failures, and per-row actions:

- **Retry** on failed uploads (re-checks conflicts first),
- **Save as** on drive REST download rows,
- **Open folder** on finished downloads.

The empty state reads: "No transfers yet — drag files onto a session
window to send them."

## Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| "Transfers disabled by this server" | Admin toggle off, or the capability probe never succeeded (fail-closed). |
| "Device not paired" | Run device pairing for the instance (Settings → Device pairing in v1.1.0, or the tray's server menu). |
| "SSH session" notice | SSH has no REST drive; use the in-session upload button. |
| "The paired token does not own this session" | Pair with the account that started the session. |
| "Token rejected" | The paired token was revoked; re-pair. |
| Upload fails with a 404 | The session ended or its drive is gone; reconnect. |
| Empty drop notice | KDE Wayland quirk; try the drag again. |
