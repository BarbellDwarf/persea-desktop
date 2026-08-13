# Capabilities baseline (D01)

`default.json` grants the minimal permission set to the `main` window's
local shell pages only:

- `core:default`: invoke, events, basic window access.
- `core:tray:default`: tray API, prepared for D08.
- `core:window:allow-*` (explicit list): window lifecycle for the
  multi-window session manager (D05) and `set-fullscreen` for kiosk
  (D12). The list stays explicit instead of `core:window:default` so
  the surface grows deliberately.

Remote origins have no capability: until D04 adds a
`remote.urls` scope limited to the persea origin, pages loaded from a
persea server can invoke nothing.

`tauri.conf.json` pins `tauri` with the `tray-icon` feature so the
`core:tray:*` permissions are registered at build time.
