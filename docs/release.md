# Cutting a release

This page covers the release pipeline (`.github/workflows/release.yml`) and the per-OS install notes for the artifacts it produces.

## How a release is built

Pushing a `v*` tag (for example `v1.0.0`) runs the `Release` workflow:

1. **Gates**: the shared CI workflow (`ci.yml`, the same checks that run on every PR) runs first: fmt, clippy, tests on Windows, Linux and macOS, plus `cargo audit`.
2. **Draft**: a draft GitHub Release is created for the tag (or reused on a rerun, with stale assets removed).
3. **Build legs**: four parallel legs build the full matrix and upload their bundles to the draft:
   - `windows-latest`: msi + nsis
   - `ubuntu-22.04`: deb + rpm + AppImage
   - `macos-latest`: dmg for aarch64, and a second leg cross-compiling the dmg for x86_64
4. **Smoke**: after the upload, each leg launches the built binary, waits 15 seconds and fails when the app process exited early. A failed leg blocks the publish step, so the release stays a draft; the leg's already-uploaded assets are replaced on the next run.
5. **Publish**: only when every leg passed, the draft is un-drafted.

The publish step runs after all four legs, so a release missing a platform leg can never ship. A failing gate or leg leaves the release as a draft, which is invisible to users. The prepare and publish jobs run `actions/checkout` before their `gh release` calls: gh needs a git context.

## Step by step: cut a release

1. Bump the version in `src-tauri/Cargo.toml` (`package.version`) and `src-tauri/tauri.conf.json` (`version`). Keep the two in sync.
2. Merge to `main` and wait for CI to be green.
3. Tag and push:

   ```
   git tag v1.0.0
   git push origin v1.0.0
   ```

4. Watch the `Release` run in Actions. When it finishes, the release page has all seven installers (two Windows, three Linux, two macOS dmgs) plus the updater files: a `latest.json` and one `.sig` signature per updater package.
5. The release notes are auto-generated from merged PRs. Edit the notes after publishing if needed; releases stay editable.

Re-running the workflow for the same tag re-drafts the release, replaces every asset and publishes again.

## Install notes per OS

- **Windows**: the NSIS installer (`*-setup.exe`) or the MSI (`*.msi`). Both are unsigned, so SmartScreen shows a warning; use "More info" then "Run anyway". The installer bootstraps the WebView2 runtime from the internet at install time.
- **Linux**:
  - `.deb` for Debian 13 and derivatives (Debian 13 ships the required webkit2gtk 4.1). The build runs on Ubuntu 22.04, so the binary's glibc floor is old enough for older Debian/Ubuntu releases too.
  - `.rpm` for RHEL 10 and derivatives with EPEL enabled; `webkit2gtk4.1` is declared as a dependency because rpm-rs adds no automatic dependency detection.
  - `.AppImage` covers distros that are neither Debian nor RHEL based. Make it executable (`chmod +x`) and run it.
- **macOS**: two dmgs, one per architecture: arm64 for Apple Silicon, x86_64 for Intel. Minimum system version is 10.15. The app is ad-hoc signed (no Apple certificate), so Gatekeeper complains: right-click the app and choose Open, or run `xattr -dr com.apple.quarantine "/Applications/Persea Desktop.app"` after copying it to Applications.

## Updater and signing

In v1.1.0 the app updates itself from the release's `latest.json` asset. It checks on startup, every 4 hours and on the manual "Check for updates" action in Settings, and offers a Download & restart flow once a newer version is found. The update installs in place: it restarts the app on Windows (NSIS/MSI) and AppImage, swaps the bundle in place on macOS, and re-installs through the package manager on deb/rpm (where a restart comes later). Update check failures are silent and never block the app.

`bundle.createUpdaterArtifacts` is on in `tauri.conf.json`, so every release also carries the updater artifacts. The workflow injects the signing keys and the tauri-action uploads `latest.json` and every `.sig` alongside the installers.

Key management:

- The minisign keypair was generated once with `npx @tauri-apps/cli signer generate`. The PUBLIC key is committed in `tauri.conf.json` under `plugins.updater.pubkey` and is shared by the stable and beta channels.
- The PRIVATE key exists only as the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets. It must never be committed or written into any file in the repo.
- Losing the private key or its password bricks updates: existing installs can never verify a signature again, so they stop updating. Keep a backup of the keypair outside the repo.
- To rotate the keypair: regenerate it, commit the new public key in `tauri.conf.json`, update the two secrets, then cut a release. Installations adopt the new public key with the release they install from it.

Beta installers never consume the stable channel; see [beta.md](beta.md).

## CI caching

Each build leg uses `rust-cache` scoped to the leg. If release legs approach the runner time limits, the plan calls for sccache (a shared cache across legs) as the next step; tune before the first long release run.
