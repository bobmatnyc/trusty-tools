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
- **Any other OS:** there is no value store. The Keychain backend is not
  built, and every operation fails with `SecretsError::UnknownBackend`. The
  crate does not fall back to a file or an in-memory store.

The only files the crate writes are the names-only index at
`~/.trusty-tools/trusty-secrets/index/`. Each file is mode 0600 in a 0700
directory. It lists key names, value lengths, update times and the "agents may
use" flag. It never holds a value.

## Scopes

| Scope | Covers | Vault name |
|---|---|---|
| Project | One repository | `trusty/<owner>/<repo>` |
| Owner | Every project of one GitHub owner | `trusty/<owner>` |

The owner and repository come from the `origin` git remote, which must be on
`github.com`; any other host is refused. A lookup for `secret://KEY` checks
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
| `store` | yes | The `SecretBackend` trait, `KeychainBackend`, `NamesIndex`, `SecretStore`, scope resolution, `mask_secret`, config resolution, the `secret://` resolver and the `.env` parser. Implies `api`. |
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
trusty-secrets serve [--socket P] [--index-dir P] [--machine-config P] [--idle-timeout-secs N]
```

The environment variables `TRUSTY_SECRETS_SOCKET`, `TRUSTY_SECRETS_INDEX_DIR`
and `TRUSTY_SECRETS_IDLE_TIMEOUT_SECS` set the same values. No method returns
a secret value. `server::OnDemandSecrets` is the helper that starts the binary
and sends a request.

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
