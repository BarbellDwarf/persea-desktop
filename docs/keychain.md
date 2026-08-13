# Keychain

The desktop shell stores the paired API token (D07) and optional saved
credentials through the OS keychain. There is no official Tauri plugin
for this, so Persea Desktop ships a custom Rust command module
(`src-tauri/src/keyring.rs`) over the keyring v4 ecosystem
(`keyring-core` plus one store crate per backend).

## Commands

Exposed only to local shell pages. The remote persea page cannot invoke
them (see [Security](#security)).

| Command | Signature | Returns |
|---|---|---|
| `keyring_set` | `(service, user, secret)` | `null` on success |
| `keyring_get` | `(service, user)` | the secret, or `null` when no entry exists |
| `keyring_delete` | `(service, user)` | `true` if a credential was deleted, `false` if none existed |
| `keyring_tier` | `()` | `{ tier, notice }`; `notice` is set only for fallback tiers |

Namespacing: every credential lives under the app service
`dev.persea.desktop`; the `user` is caller-supplied and D07 uses
`<instance url> + device token id`. The commands accept `service` and
`user` as parameters so future features can use separate namespaces
without an API change, but the shell should pass `dev.persea.desktop`
for the service.

## Store matrix

| Platform | Store | Persistence | Notice |
|---|---|---|---|
| macOS | Keychain Services (login keychain) | until deleted | none |
| Windows | Credential Manager | until deleted | none |
| Linux | Secret Service (gnome-keyring, KWallet, KeePassXC) via zbus | until deleted | none |
| Linux | `db-keystore` (encrypted sqlite file, fallback) | until deleted | "stored less securely" |
| Linux | `keyutils` (kernel session keyring, fallback) | until reboot or logout | "stored less securely" |

The store is selected once per process at the first keyring command.

## Linux fallback tiers

At runtime the module walks a fallback chain and keeps the first tier
whose construction and probe round-trip (set, get, delete of a throwaway
credential) succeed:

1. **Secret Service**. The zbus connection is blocking; every call runs
   on a blocking thread (`tauri::async_runtime::spawn_blocking`), never
   on the main thread. A blocked or locked keyring daemon fails the
   probe and triggers the fallback.
2. **db-keystore**. An sqlite database (Turso) encrypted with
   AES-256-GCM, at `<app data dir>/keyring.db`. The 256-bit key is
   generated randomly on first use and stored at
   `<app data dir>/keyring-key`, both files `0600`, directory `0700`.
   The key cannot be derived from the pairing token (the pairing token
   is what this store protects, so deriving from it would be circular)
   and no passphrase prompt exists in the D06 shell scope, so the
   decision is a random per-install key. This protects against casual
   file reads by other local users, not against an attacker with the
   same account.
3. **keyutils**. The kernel session keyring. Credentials vanish when the
   session ends, which makes stored pairing tokens useless after a
   reboot.

The winning tier is persisted to `<app data dir>/keyring-tier`. On the
next startup the chain starts at that tier and only walks downward, so
a temporarily missing Secret Service daemon does not cause credentials
to bounce between stores and disappear.

### Notice text

The shell calls `keyring_tier` and shows `notice` when it is set:

> db-keystore: "The desktop keyring is unavailable, so credentials are
> stored in an encrypted file on this device. They are less protected
> than a system keychain."
>
> keyutils: "The desktop keyring is unavailable, so credentials are
> stored in the Linux kernel session keyring and are lost when this
> session ends."

## Security

- **Remote origin denial.** The four commands are declared in the app
  manifest (`tauri_build::AppManifest::commands`), which moves them from
  "allowed by default" into the ACL. Only `capabilities/default.json`
  grants them, to the `main` window's local pages. The Tauri IPC layer
  rejects any invocation without a matching ACL entry, and separately
  rejects every custom command invoked from a remote origin even without
  the manifest. The keyring commands are thus unreachable from a persea
  server page (defense in depth on top of D04's origin scoping).
- **No main-thread keyring calls.** All four commands run their keyring
  work inside `spawn_blocking`. The zbus Secret Service backend must
  never touch the main thread: its blocking connection deadlocks against
  the GTK main loop.
- **Secrets in memory.** The secret lives in the command arguments and
  in the process heap like any other string. Keyring entries are deleted
  with `keyring_delete` when a pairing token is revoked.
- **Fallback honesty.** The fallback tiers are less secure than the OS
  stores, which is why the shell surfaces the notice; the tier is
  reported by `keyring_tier` on every startup, not just at pairing time.

## Verification

- `cargo test` in `src-tauri`: mock-store unit tests cover
  set/get/delete round-trips, missing entries, the fallback chain,
  marker stickiness, probe failure, and input validation.
- Manual per-OS verification (definition of done):
  1. pair with a persea server, restart the app, confirm the token
     survives,
  2. Linux: stop the Secret Service daemon (or run headless) and confirm
     the fallback engages and the notice appears; confirm credentials
     survive an app restart while the daemon stays unavailable, and do
     not silently move back to the Secret Service,
  3. Linux with no Secret Service and no writable home: confirm the
     keyutils fallback and the notice,
  4. confirm a remote page calling `keyring_get` receives a permission
     error and the local shell still works.

## Dispatcher wiring

The module does not register itself. Required changes (also summarized
in the module doc comment in `src/keyring.rs`):

1. `src-tauri/Cargo.toml`: the pre-wired `keyring = { version = "4", ... }`
   line does not compile and must be replaced. Verified against the
   vendored crates: keyring 4.x requires its `v1` or `cli` feature or it
   emits `compile_error!`; `apple-native-keyring-store` requires the
   `keychain` feature or it emits `compile_error!` on macOS; and
   `secret-service` requires a runtime feature. The store crates are
   also target-gated, because the apple store drags in
   `core-foundation` 0.10.1, which has ungated `std::os::unix` imports
   and fails to compile on Windows otherwise:
   ```toml
   keyring-core = "1.0.0"

   [target.'cfg(target_os = "macos")'.dependencies]
   apple-native-keyring-store = { version = "1.0.2", features = ["keychain"] }

   [target.'cfg(target_os = "windows")'.dependencies]
   windows-native-keyring-store = "1.1.0"

   [target.'cfg(target_os = "linux")'.dependencies]
   zbus-secret-service-keyring-store = { version = "1.0.0", features = ["rt-tokio-crypto-rust"] }
   linux-keyutils-keyring-store = "1.0.0"
   db-keystore = "0.4.3"
   ```
   This is the architecture the keyring README prescribes for
   applications that select stores at runtime; only the platform's own
   store crates are compiled into each binary. `rt-tokio-crypto-rust`
   matches the Tauri tokio runtime that backs `spawn_blocking`.
2. `src-tauri/build.rs`: gate the commands (see `src/keyring.rs`).
3. `src-tauri/src/lib.rs`: `mod keyring;` and
   `tauri::generate_handler![keyring::keyring_set, keyring::keyring_get,
   keyring::keyring_delete, keyring::keyring_tier]` in
   `invoke_handler`.
4. `src-tauri/capabilities/default.json`: add `allow-keyring-set`,
   `allow-keyring-get`, `allow-keyring-delete`, `allow-keyring-tier` to
   the `main` window's permission list.
