# Beta channel

The desktop beta mirrors the server's rolling beta: a manual workflow builds the full artifact matrix and publishes it to a `beta` pre-release, replacing the previous beta each time. This page explains how to run it and how to become a beta tester.

## How a beta run works

The `Beta Desktop` workflow (`.github/workflows/beta.yml`) is dispatch-only, like the server beta:

1. Choose a branch in the dispatch dialog (the branch picker). The build runs on that branch, no merge to `main` needed.
2. **Gates**: the shared CI checks (fmt, clippy, tests on all three OSes, `cargo audit`) and CodeQL run first. A failing check aborts the run before anything is published.
3. **Prepare**: the previous `beta` release and its moving tag are deleted, and a new draft pre-release is created at the dispatched branch head.
4. **Build legs**: the same four legs as the release workflow (msi + nsis, deb + rpm + AppImage, dmg arm64, dmg x86_64), each with the launch smoke test, uploading to the draft.
5. **Finalize**: once every leg passed, the pre-release is un-drafted.

If a leg fails, the run leaves only a draft: beta testers never see a broken build, but the previous beta is gone until a successful rerun. Dispatchers should check the run result before telling testers to grab the new build.

Beta builds are versioned `X.Y.Z-beta.<run number>` (for example `1.2.0-beta.42`), derived from the `tauri.conf.json` version at prepare time, so the updater sorts them correctly within the beta channel.

## Becoming a beta tester

1. Ask a maintainer to dispatch the `Beta Desktop` workflow on the branch under test, or dispatch it yourself if you have write access.
2. Open the `beta` pre-release from the release page once the run finishes.
3. Download the installer for your OS (same matrix as the stable release) and install it.

While the updater is wired, beta installs update automatically from the beta channel: install the first beta manually, and every following beta lands through the updater.

## Channel semantics

- The updater endpoints are baked into a beta build at build time and point at the beta release's `latest.json` (`https://github.com/persea-grove/persea-desktop/releases/download/beta/latest.json`) forever. A beta install keeps updating from the beta channel until it is replaced.
- Stable installers point at the stable channel and never auto-update to a beta.
- Leaving the beta channel means installing the stable installer, which then updates from stable. There is no automatic channel switch in v1.2.0.
- Because the `beta` tag is deleted and recreated on every run, the beta release's asset URLs are only valid between runs. The previous `latest.json` disappears with the previous release.

## Updater endpoints

The beta workflow passes the beta endpoint as a build-time `--config` override. The stable endpoints live in `tauri.conf.json`. Both channels are signed with the same minisign keypair: the public key is committed in `tauri.conf.json` (`plugins.updater.pubkey`), the private key lives only in the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets; see [release.md](release.md) for key management and rotation.
