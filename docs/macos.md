# macOS notes: signing, Gatekeeper, updates

Persea Desktop ships ad-hoc signed for macOS 14/15 (arm64 and x86_64).
This page explains what that means in practice, what the Gatekeeper
prompts are, and what the notarization follow-up will change.

## Signing posture

The app is signed with an ad-hoc identity (`signingIdentity: "-"`):
`codesign --sign -`, no developer certificate, no notarization. Ad-hoc
signing is the minimum viable posture because Apple Silicon refuses to
execute a binary with no code signature at all. The bundle also carries
an entitlements file (`src-tauri/entitlements.plist`) with the
WebKit-related allowances (JIT, unsigned executable memory, library
validation). Those matter once hardened runtime + notarization land;
they are inert under the ad-hoc signature.

What ad-hoc signing does not give you:

- No Gatekeeper trust: the first launch of a downloaded app shows the
  "cannot be opened because the developer cannot be verified" warning.
- No notarization, so no automatic trust after the first bypass.
- No identity, so every rebuilt artifact behaves like a fresh unknown
  app.

## Opening the app for the first time

A downloaded `.dmg` lands the app in the "unidentified developer"
category. Two ways past the warning:

1. Right-click (or Ctrl+click) the app in Finder → **Open** → **Open**
   in the dialog. This records an exception for that exact binary.
2. System Settings → Privacy & Security → scroll to the security
   section → **Open Anyway** next to the Persea Desktop entry.

**macOS 15 (Sequoia) is stricter.** Apple has been tightening the
bypass paths since macOS 15; the right-click route can fail or behave
differently there, and the "Open Anyway" entry in Privacy & Security is
the more reliable path. If neither works on your macOS version, the
real fix is notarization (roadmap below). Installing from the command
line (`open` on the dmg, or `xattr -dr com.apple.quarantine` on the
app) is the developer-grade bypass; the quarantine attribute is what
triggers the prompt, and removing it is the same trust decision the UI
dialog makes.

## Updates re-prompt Gatekeeper

The updater replaces the `.app` bundle with the downloaded one. The
replacement carries a fresh ad-hoc signature and a fresh quarantine
state, so **every update re-triggers the Gatekeeper prompt**. This is
expected behavior for ad-hoc signed apps: the minisign signature (what
the updater verifies) and Gatekeeper (what macOS verifies) are
independent mechanisms. Until notarization lands, budget one bypass per
update. Test updates on a machine you can afford to click through.

## Fullscreen

Shell-level fullscreen works natively: the View menu's Fullscreen item
and expand-to-monitor both toggle the window's fullscreen state through
the macOS window API.

Element fullscreen (the web page calling `requestFullscreen` for video
or media, which the remote UI's own fullscreen button uses) is a
different mechanism: WKWebView needs a private API (`fullScreenEnabled`)
that is gated behind the `macos-private-api` Tauri feature, which is
**not enabled** in 1.2.0. If a page's element fullscreen does nothing,
use the shell fullscreen instead. Enabling the feature is a one-line
`Cargo.toml` change (add the `macos-private-api` feature to the `tauri`
dependency) plus a rebuild; it was deferred because private APIs are
banned from the Mac App Store and the feature flag makes the trade-off
explicit.

## Notarization roadmap (follow-up work)

Notarization removes the Gatekeeper prompts and the per-update
re-prompts. It requires an Apple Developer Program membership
($99/year). When the time comes, this is the work:

1. **Account + certificate.** Enroll in the Apple Developer Program,
   generate a Developer ID Application certificate (in Xcode → Settings
   → Accounts, or the Apple developer portal), and export it to the
   build machine's keychain. Record the certificate name; it becomes
   the signing identity.
2. **Bundle config.** Replace `signingIdentity: "-"` with the
   certificate name (CI: `APPLE_SIGNING_IDENTITY`), keep the hardened
   runtime on, keep the entitlements file (it already carries the
   WebKit allowances hardened runtime requires).
3. **Notarize + staple.** `xcrun notarytool submit` the app archive,
   then `xcrun stapler staple` the `.app`. Tauri's bundler can
   notarize the `.app` during `tauri build` (via `APPLE_ID`,
   `APPLE_PASSWORD`/app-specific password, `APPLE_TEAM_ID`), but it
   does **not** notarize the `.dmg`: a separate `notarytool` +
   `stapler` step for the dmg is required, or the dmg keeps warning.
4. **CI secrets.** Store the certificate (and password), `APPLE_ID`,
   the app-specific password and `APPLE_TEAM_ID` as CI secrets; never
   put them in the repo.
5. **Acceptance.** On a clean macOS 14 and 15 machine (arm64 and
   x86_64): first install with no Gatekeeper prompts, update through
   the updater with no prompts, `spctl -a -vv` reports "accepted".

Until then: expect the warnings, use right-click-open or Open Anyway,
and do not file the prompts as bugs. They are the documented price of
not paying the $99/year.
