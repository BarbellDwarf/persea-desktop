# Provisioning: enterprise server injection

Enterprises want the installer to pre-configure the desktop shell: install
the app, first launch, the server instance is already there (name + URL),
optionally with kiosk mode and settings overrides pinned. Provisioning is
the contract that makes that work on every platform: one JSON document, a
fixed source order, and per-platform delivery paths.

This document is the contract. The packaging side (bundle baking, NSIS/MSI
custom actions, postinst hooks, MDM payloads) implements the delivery; the
shell implements the reading, locking, and re-sync. Anything not specified
here is unspecified: keep the document the single point of truth.

## The provision document

One JSON document, UTF-8:

```json
{
  "instances": [
    { "name": "Production", "url": "https://persea.example.com", "default": true },
    { "name": "Lab", "url": "https://lab.example.com", "default": false }
  ],
  "kiosk": { "enabled": false },
  "settings": { "appearance": "dark" }
}
```

All three sections are optional; an empty object `{}` is valid.

### `instances`

A list of server instances. Each entry has exactly the fields of the shell's
instance store:

| Field     | Type    | Required | Meaning                                        |
|-----------|---------|----------|------------------------------------------------|
| `name`    | string  | yes      | Display name (non-empty after trimming)        |
| `url`     | string  | yes      | `https://...` (plain `http` only for localhost) |
| `default` | boolean | no       | Marks the default instance (default `false`)   |

URL rules match the shell's own validation: https only, http accepted for
localhost/127.0.0.1/::1, no empty host, trailing slashes stripped. URLs must
be unique within the document.

Validation is all-or-nothing: a document with one bad entry is rejected as a
whole ("no half-imports"). A rejected source is logged and ignored.

### `kiosk`

```json
{ "kiosk": { "enabled": false } }
```

| Field     | Type    | Meaning                                                        |
|-----------|---------|----------------------------------------------------------------|
| `enabled` | boolean | `true` pins kiosk on, `false` pins it off. Absent = not governed |

An absent `kiosk` section leaves kiosk to the user setting. A present
`enabled` value is a locked override: provision > user setting > default,
and the user cannot change it while pinned.

### `settings`

An arbitrary JSON object of shell-settings overrides, keyed by the shell
setting name. Example keys: `appearance` (`"auto" | "light" | "dark"`),
`shortcuts`. Every key present is a locked override: provision > user
setting > default. The settings page merges these over the user settings
and hides the controls for overridden keys. Unknown keys are accepted and
ignored (forward compatibility); `settings` must be an object when present.

## Source order and trust

First valid source wins. Sources are consulted in this order; the first
that exists, passes its trust check, and parses wins. Sources that are
missing, unreadable, untrusted, or invalid are logged and ignored, and the
chain continues to the next one. No valid source = no provisioning, the app
launches as usual.

| # | Source | Path / location | Trust |
|---|--------|-----------------|-------|
| 1 | App bundle | `Resources/provision.json` (build-time bake, all platforms) | Built by the installer, shipped with the app: trusted |
| 2 | Windows machine policy | `HKLM\Software\Persea Desktop\Provisioning`, registry value `config` (REG_SZ, JSON string) | HKLM is admin-only: trusted |
| 3 | Linux machine file | `/etc/persea-desktop/provision.json` | Honored only when owned by uid 0 or the root group (gid 0) |
| 4 | macOS machine file | `/Library/Application Support/Persea Desktop/provision.json` | Honored only when owned by uid 0 or the root group (gid 0) |

### Windows fallback (documented, not implemented)

A `ProgramData` file fallback (`C:\ProgramData\Persea
Desktop\provision.json`) is deliberately NOT implemented. `ProgramData` is
user-writable by default, so a file there is lower trust: a local user could
plant a provision file that redirects the app to a phishing server. If a
file fallback is ever added, it must enforce an explicit ownership/ACL
check (only Administrators / SYSTEM can write) and be documented as
lower-trust than HKLM. HKLM is the trusted Windows delivery path.

### Linux ownership rule

The file is honored only when `uid == 0 || gid == 0` (root, or the root
group). Installers run as root and write the file root-owned; anything
else (a user-writable drop, a world-writable temp copy) is logged and
ignored. This is a tamper check, not a completeness check: apply it after
writing the file, in the same step.

### macOS ownership rule

Same rule as Linux: `uid == 0 || gid == 0`. Installer packages (pkg
scripts, MDM payloads) write root-owned files; that is what the rule
accepts.

## Locked instances

Provisioned instances are applied with `locked = true`. The lock follows
the provision entry:

- The settings UI hides edit, remove, and set-default for locked entries.
- The commands refuse them with "This instance is locked by your
  administrator".
- Opening and connecting work normally, including the startup auto-open.

The lock is not part of the provision schema: every applied entry is locked
by construction.

## Re-sync on every launch

Every launch, the shell reads the effective provision document and hashes
its canonical serialization (SHA-256 of the re-serialized parsed document,
so whitespace or formatting changes never trigger a merge). The hash is
persisted on the instance store (`instances.json`, `provisionedHash`).

- **Unchanged hash**: the merge is skipped entirely. No writes, no churn.
- **Changed hash**: merge, then adopt the new hash and save.

The merge:

1. **ADD** provision entries whose URL is not yet in the store (locked).
2. **UPDATE** name and default of locked entries that match a provision
   URL.
3. **REMOVE** locked entries whose URL is no longer provisioned
   (last-used is cleared if it pointed at a removed entry).
4. User-added entries are never touched: an unlocked entry holding a
   provisioned URL is left alone (the provision entry for that URL is
   skipped), and removal only ever targets locked entries.
5. A provisioned default wins: when the document marks an entry default,
   every other instance's default flag is cleared, so the store keeps a
   single default.

URL changes arrive as remove (old URL) + add (new URL): entries are keyed
by URL, matching the store's uniqueness invariant.

Removing the provision source does NOT unlock or remove previously applied
entries. The lock follows the provision entry, not the file: a launch with
no source is a no-op. Un-provisioning a machine requires a new provision
document that no longer lists the entries (then the next launch removes
them).

Newly added instances are probed like any other instance (reachability,
version, capabilities) in the normal background pass.

## Build-time bake flow

`scripts/package.sh --provision <file>`:

1. Validate `<file>` with the same rules the shell applies (JSON schema,
   URL rules, unique URLs, non-empty names). Reject with a non-zero exit on
   any violation.
2. Copy the file to `Resources/provision.json` inside the app bundle, for
   every platform target (macOS `Contents/Resources`, Windows and Linux
   resource directories as resolved by the shell at runtime).
3. Without `--provision`, no `provision.json` is baked (the app must not
   ship an empty or default one).

The shell reads `resource_dir()/provision.json` at runtime; the packager
must ensure the file lands exactly there. Because the bundle source
outranks the machine locations, do not bake AND set a machine policy on
the same machine unless the bake is intended to win.

Alternative Windows delivery: NSIS/MSI custom actions writing the HKLM
value (a JSON string, REG_SZ) are equivalent to the bake and win over it
only when no bake exists, per the source order.

## Security notes

- Never put credentials in a provision document: HKLM is admin-readable,
  bundle resources and machine files are world-readable. Provisioning
  carries server locations and preferences, nothing secret.
- Trust is per-source, checked before parsing: bundle = installer-shipped,
  HKLM = admin-only, machine files = root-owned. Everything else is
  ignored with a log line.
- Invalid or untrusted sources never crash the app and never produce
  half-imports: the whole source is rejected, the chain continues, and the
  app launches.
- Locked entries give users open/connect only; the commands refuse edits,
  and the UI hides the controls. This is enforced in the shell commands,
  not just the UI.
- Kiosk and settings overrides follow the same model: presence of an
  override in the provision document is the lock.

## Verification per platform

| Check | How |
|-------|-----|
| Bundle bake | `scripts/package.sh --provision prov.json`, install, launch: instance present, locked, connect works |
| Windows HKLM | `reg add "HKLM\Software\Persea Desktop\Provisioning" /v config /t REG_SZ /d "<json>" /f`, launch |
| Linux file | root-owned file at `/etc/persea-desktop/provision.json`, launch |
| macOS file | root-owned file at `/Library/Application Support/Persea Desktop/provision.json`, launch |
| Idempotence | Launch twice: `instances.json` mtime unchanged on the second launch |
| Re-sync | Edit the source (name/URL/add/remove), launch: store follows; user-added instances untouched |
| Lock | Locked entry shows no edit/remove in settings; connect works; command-level refusal |
| Invalid source | Corrupt the JSON / chown to a normal user / remove the file: launch is clean, entry untouched |
| Source order | Bake + machine policy with different content: bundle wins |
