# trusty-secrets

[![crates.io](https://img.shields.io/crates/v/trusty-secrets.svg)](https://crates.io/crates/trusty-secrets)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Project- and owner-scoped secret storage for the trusty-* tools. The crate
holds the key, scope and `secret://` reference types, a names-only index, a
macOS Keychain backend, and an on-demand Unix socket server.

Version 0.1.0 is the first release. It ships as a crate only. The `tm secrets`
CLI and the MCP tool that sit on top of it ship in a later trusty-mpm release
and are not available yet.

Design: [DOC-74](../../docs/specs/DOC-74-secrets-integration.md) section 15.
Product intent: [PRD-SECRETS-01](../../docs/prd/PRD-SECRETS-01-console-secrets.md).
Tracking: [#9073](https://github.com/bobmatnyc/trusty-tools/issues/9073).

## Where values live

- **macOS:** the Keychain. A secret is one Keychain entry: the service is
  `trusty/<owner>/<repo>` (or `trusty/<owner>` for an owner secret) and the
  account is the key name. Nothing is cached. Every read goes to the OS.
- **Any other Unix:** the file backend (`file`). The Keychain backend is not
  built here, so `file` is the default. Each value is a plaintext file at
  `~/.trusty-tools/trusty-secrets/values/<vault>/<key>`, mode 0600 in 0700
  directories, created with that mode and never widened. Every operation
  refuses a value file or directory that is a symlink, grants more than
  0600/0700, or belongs to another user (`SecretsError::StorageRefused`).
  `secrets.doctor` reports this posture as `file_degraded`.
- **macOS with `secrets.default_backend: file`:** the file backend is used
  only when the untracked machine config
  (`~/.trusty-tools/trusty-common/config.yaml`) names it. A tracked project
  config naming `backend: file` is refused with
  `SecretsError::TrackedBackendRefused`. A failing Keychain never falls back
  to files. The server writes a value into `file` only when the user's own
  machine config selects it: `.trusty-tools/trusty-common/config.yaml` under
  the home directory the password database records for the server's user.
  A config named with `serve --machine-config`, or found under a different
  `$HOME`, does not count. A `set` or a `copy` into `file` without that
  selection — or when that home cannot be looked up, or the file cannot be
  read or parsed — is refused with `SecretsError::FileBackendNotSelected`
  (wire kind `file_backend_not_selected`), opens no backend, writes nothing,
  and leaves one audit denial. Reading from `file` and deleting from it stay
  allowed.

An explicitly configured `keychain` stays the Keychain on every host; off
macOS it fails with `SecretsError::UnknownBackend`.

Apart from value files, the crate writes only the names-only index at
`~/.trusty-tools/trusty-secrets/index/`. Each file is mode 0600 in a 0700
directory. It lists key names, value lengths, update times and the "agents may
use" flag. It never holds a value.

## Scopes

| Scope | Covers | Vault name |
|---|---|---|
| Project | One repository | `trusty/<owner>/<repo>` |
| Owner | Every project of one GitHub owner | `trusty/<owner>` |

The owner and repository come from the `origin` git remote, which must be a
`github.com` https, ssh or scp-form URL; any other host or scheme is refused. A lookup for `secret://KEY` checks
the project vault first, then the owner vault. If the scope cannot be
determined, the call fails. It never guesses.

A `secrets.vault` override in the tracked `.trusty-tools/trusty-secrets.yaml`
may name only `trusty/<owner>/<name>` under the remote's owner. To point a
checkout at any other vault, add it to the machine config instead
(`~/.trusty-tools/trusty-common/config.yaml`, `secrets.project_vaults`, keyed
by `<owner>/<repo>`). See DOC-74 §6.1.

## References

A reference names a secret without carrying its value.

| Form | Resolves in |
|---|---|
| `secret://KEY` | project vault, then owner vault |
| `secret://<owner>/KEY` | the owner vault |
| `secret://<owner>/<repo>/KEY` | that project vault |

An explicit form may name only the caller's own project or owner vault. Any
other vault is refused before anything is read.

A key is 1 to 256 characters of `[A-Za-z0-9_.-]`, starting with a letter,
digit or `_`.

## Features

| Feature | Default | What it adds |
|---|---|---|
| `api` | yes | Validated names, `SecretRef`, the redacting `SecretValue`, the `secrets.*` request and response types, `SecretsError`. No store code. |
| `store` | yes | The `SecretBackend` trait, `KeychainBackend`, `FileBackend` (Unix), `NamesIndex`, `SecretStore`, scope resolution, `mask_secret`, config resolution, the `secret://` resolver and the `.env` parser. Implies `api`. |
| `server` | yes | The on-demand Unix socket and the `trusty-secrets` binary. Unix only. Implies `store`. |
| `test-support` | no | `MemoryBackend`, an in-memory backend for tests. Implies `store`. |

A caller that only names keys can depend on `api` alone:

```toml
trusty-secrets = { version = "0.1", default-features = false, features = ["api"] }
```

## Install

```bash
cargo install trusty-secrets --version 0.1.0 --locked
```

This installs the `trusty-secrets` binary.

## The on-demand server

`trusty-secrets serve` answers the `secrets.*` methods (`scopes`, `list`,
`set`, `delete`, `copy`, `doctor`) on a Unix socket at
`~/.trusty-tools/trusty-secrets/secrets.sock`. A client starts it on the first
call. It exits after 60 seconds with no answered request and removes its
socket. No launchd job runs it.

```
trusty-secrets serve [--socket P] [--index-dir P] [--machine-config P] [--audit-log P] [--idle-timeout-secs N]
```

The environment variables `TRUSTY_SECRETS_SOCKET`, `TRUSTY_SECRETS_INDEX_DIR`
and `TRUSTY_SECRETS_IDLE_TIMEOUT_SECS` set the same values, with one limit:
`TRUSTY_SECRETS_INDEX_DIR` is read only by a server whose socket is not the
default one, under `$HOME` or under the home directory the password database
records for the server's user. A client passes its environment to the server it starts, and that
server answers every client of the default socket, so one caller's environment
must not move the names index for all of them. A test or sandbox on its own
socket keeps the variable, and `--index-dir` works on any socket. Only flags
move the audit log: `--audit-log`, or `--index-dir`, which puts it beside the
index. No environment variable moves it, including `TRUSTY_SECRETS_INDEX_DIR`.

The project config `.trusty-tools/trusty-secrets.yaml` must be a regular file
of at most 64 KiB, and not a symlink. Anything else is refused before it is
read. No method returns
a secret value. `server::OnDemandSecrets` is the helper that starts the binary
and sends a request.

`delete` removes the key from every backend this build can store values in
(the Keychain on macOS, and the file backend), not only the configured one, so
a value left behind by a backend switch or a `copy` is removed too. If any
backend fails to delete, the call fails and the key stays listed.

`copy` moves keys between two backends of the same project. On macOS its
destination may be `file` only when the user's own machine config, at its
fixed location (see above), sets `secrets.default_backend: file`. Any process
running as the same user can call the socket, and can start a server with
its own `--machine-config` or `$HOME`, so without that rule it could move
Keychain values into plaintext files.

## Audit trail

The server records each credential access in
`~/.trusty-tools/trusty-secrets/audit/audit.jsonl`, one JSON line per record
(DOC-45 §9). Every record has `"stream": "credential_access"`.

| Method | Records |
|---|---|
| `set`, `delete` | One per call, allowed or denied |
| `copy` | One per key, plus one denial for a refusal before any key moves |
| `list` | One per denied call only |
| `scopes`, `doctor` | None |

A record holds `ts`, `stream`, `method`, `decision` (`allow` or `deny`),
`reason` (the error kind, such as `vault_out_of_scope`), `vault`, `key`,
`backend`, `project_root` and `caller_pid`. It never holds a value or any text
the caller sent.

The log is a 0600 file in a 0700 directory. The server refuses it if it is a
symlink, has a wider mode, or belongs to another user. Each record is synced
before the reply. When the log reaches 8 MiB it is renamed to `audit.jsonl.1`
the next time it is opened.

The server opens the log before an allowed `set`, `delete` or `copy` touches
a backend. If the log cannot be opened, the call fails with `audit_unavailable`
and changes nothing. If the record cannot be written after the backend call
succeeded, the change stands and the call still fails with
`audit_unavailable`, so no success is reported without its record; `copy` stops
before its next key. A refusal is still answered with its own error. To turn the audit off on one machine, set `secrets.audit: false` in
the machine config `~/.trusty-tools/trusty-common/config.yaml`. A project's
tracked `.trusty-tools/trusty-secrets.yaml` cannot do this: `audit: false`
there is refused with `tracked_audit_refused`.

## Library example

This uses the in-memory backend, so it needs the `test-support` feature and
touches no Keychain. In production, pass `Arc::new(KeychainBackend::new())` and
`NamesIndex::default_location()?`.

```toml
trusty-secrets = { version = "0.1", features = ["test-support"] }
```

```rust
use std::sync::Arc;

use trusty_secrets::store::{MemoryBackend, NamesIndex, ScopeSet, SecretStore};
use trusty_secrets::{OwnerName, RepoName, SecretKey, SecretRef, SecretValue};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index_dir = std::env::temp_dir().join("trusty-secrets-example");
    let store = SecretStore::new(Arc::new(MemoryBackend::new()), NamesIndex::at(index_dir));

    let scopes = ScopeSet::from_identity(&OwnerName::new("acme")?, &RepoName::new("web")?, None)?;
    let key = SecretKey::new("API_KEY")?;

    let set = store.set(scopes.project(), &key, &SecretValue::new("sk-live-0123456789"))?;
    println!("{}", set.masked); // sk-live-… [18 chars]

    let reference = SecretRef::parse("secret://API_KEY")?;
    let value = store.read(&reference, &scopes)?;
    assert_eq!(value.char_len(), 18);
    Ok(())
}
```

## Masking

After a write, `mask_secret` shows the value once so you can confirm the right
one went in. A value of 8 characters or fewer shows only its length, as
`[N chars]`. A longer value shows the first 8 characters, then `[N chars]`.
`secrets.list` never shows characters. It reports length and update time only.

## Safety rules

- No item in the crate prints, logs or formats a secret value. `Debug` output
  and error text carry names and locations only. `SecretValue` redacts itself.
- A value read with `SecretValue::expose` is yours to protect. Do not print it
  or put it in a log.
- Keep values out of agent transcripts. An agent session records everything
  typed into it, and the record goes to the model provider. Refer to a secret
  by key name or `secret://` reference, never by value.
- Each indexed key carries an "agents may use" flag, off by default.
  `store::resolve_reference` with `agent_parent` set refuses a key whose flag is
  off, before it reads the backend.

## Licence

MIT.
