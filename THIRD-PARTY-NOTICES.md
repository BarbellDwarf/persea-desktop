# Third Party Notices

Persea Desktop embeds or depends on the following third party software.

## Webview runtimes

Persea Desktop is a Tauri 2 application. It does not ship a webview, it
uses the runtime provided by the operating system:

- **Microsoft Edge WebView2** (Windows). Proprietary Microsoft software,
  distributed under the Microsoft Edge WebView2 Runtime license. The
  Evergreen runtime is preinstalled on Windows 11 and rolled out to most
  Windows 10 devices; the installer can bootstrap it otherwise.
  https://www.microsoft.com/edge/webview2
- **WebKitGTK** (Linux). The web content engine from the WebKit project,
  licensed under LGPL-2.1+ and BSD-2-Clause. Tauri 2 requires the 4.1 API
  (libsoup3 based). https://webkitgtk.org
- **WKWebView** (macOS). Apple's web content engine, part of macOS.
  https://developer.apple.com/documentation/webkit/wkwebview

## Rust crates

The binary is built from the crates pinned in `src-tauri/Cargo.lock`.
The most significant of them is the Tauri framework itself (MIT OR
Apache-2.0): https://github.com/tauri-apps/tauri

A machine-generated, complete dependency attribution list is a D17
deliverable; this file is a placeholder until then.
