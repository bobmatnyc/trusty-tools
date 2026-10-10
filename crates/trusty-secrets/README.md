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
| `cli-backends` | no | The CLI runner (`store::cli`), the 1Password backend (`store::onepassword`) and the Keeper backend (`store::keeper`). Unix only. Implies `store`. |
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

Each request has one deadline: 120 s for `set`, `delete` and `copy`, 15 s for
the rest. A CLI call does not start after it, and one still running is
killed; the request then fails with `deadline_exceeded`, whose text says a
write already under way may have landed. A `copy` lists in `failed` the keys
it did not start and any key whose write the deadline cut short, which may
have landed. `OnDemandSecrets` waits 15 s longer than the deadline, so for a
CLI-backed call (1Password, Keeper) it always receives the server's answer.
Each Keychain call has a time limit, `KEYCHAIN_CALL_TIMEOUT` (60 s), cut to
the request's deadline when that is sooner. A call blocked on an unanswered
Keychain access prompt fails with `backend_timeout` (`SecretsError::Timeout`
in the library), never as a miss or a success. The call is abandoned, not
stopped: its thread stays blocked until the prompt ends, and a write it
started may still land then. Until it ends, every later call on the same
Keychain item fails at once with `backend_timeout`, so that late write cannot
replace a newer one. A file call is not bounded by the deadline, but the
request is: 2 s past its deadline it answers `deadline_exceeded` while the
call keeps its thread until it returns (#9572). At most 64 method calls run at
once; a request that waits for a free slot past its deadline also answers
`deadline_exceeded`, so calls that never return hold at most 64 threads.

`delete` removes the key from every backend this build can store values in
(the Keychain on macOS, the file backend, and 1Password or Keeper when the
machine config enables it), not only the configured one, so a value left behind by a backend
switch or a `copy` is removed too. If any backend fails to delete, the call
fails and the key stays listed. On macOS this includes the Keychain when the
project is configured for `file`, so a locked Keychain fails that delete closed:
unlock the Keychain and retry.

## Doctor

`secrets.doctor` (and `tm secrets doctor`) reports the socket, the index, the
machine config the selected backend comes from, the account's own machine
config (the one file that can enable 1Password or Keeper), the selected
backend and its posture, and one row per backend: `keychain`, `file`,
`onepassword` and `keeper`, on every build. A row the server cannot open says
why, as `reason` plus a `detail` with the fix:

| `reason` | Meaning |
|---|---|
| `not_compiled` | This build does not link it: no Keychain off macOS, or no `cli-backends` feature. |
| `not_enabled` | The account's own machine config does not enable it, or the account's home is unknown. |
| `cli_not_installed` | Its CLI is missing, or `program` is not an executable file. |
| `config_invalid` | The account's machine config or the backend's section is unreadable, does not parse, or is refused (a relative `program`, a Keeper `config_path` that is missing or not mode 0600). |
| `tracked_setting_refused` | The project's tracked config sets something only the machine config may; every request in that project is refused. Shown on the selected row. |

`available` means the server opens the backend now. Doctor runs no CLI, so
it never knows whether 1Password or Keeper is unlocked. `headless` says only
whether `OP_SERVICE_ACCOUNT_TOKEN` was set when the server started: yes or
no, never the token, its length or a prefix. Keeper's device approval and
persistent login are not detected; a headless Keeper call made before that
human step fails as `backend_locked`. Doctor reads no secret, spawns no
process and writes no audit record.

`tools` lists secrets tools trusty-secrets has no backend for — `bw`,
`vault`, `pass`, `gopass`, `doppler` and `infisical` — as installed or not,
with the path found. The lookup searches only the absolute entries of the
`PATH` the server started with, and runs nothing it finds.

## 1Password

With the `cli-backends` feature (Unix), the `onepassword` backend keeps values
in 1Password through its CLI, `op`. Only the machine config can enable it:

```yaml
secrets:
  default_backend: onepassword   # or keep another default and add the section
  onepassword:
    account: my.1password.com    # optional: `op --account`
    config_path: /abs/op/config  # optional: `op --config`, an absolute path
    program: /opt/homebrew/bin/op  # optional: `op` itself, an absolute path
```

`onepassword: {}` enables it with no settings. A project file may then select
it with `secrets.backend: onepassword`. It may not set `account`,
`config_path` or `program`.

The backend runs `op` by absolute path only, found once when the backend
opens. `program` names it as given: it must be an absolute path to an
executable file, and it overrides everything else. Without `program`, the
backend takes the first executable `op` in a fixed list of system
directories: `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` on macOS, and
`/usr/local/bin`, `/usr/bin` elsewhere. It never searches the server's
`PATH`, which is whatever the spawning process had. With no such `op`, calls
fail with `cli_not_installed`; install `op` in one of those directories or set
`program`.

- The trusty vault name is the 1Password vault's name, for example
  `trusty/acme/web`. Create that vault first.
- A key is a Password item titled with the key. Its `password` field holds the
  value. An item of another category with that title is never touched.
- The value reaches `op` only on stdin for a new item, or in a 0600 template
  file for an existing one. The file is removed after the call, and a crashed
  server's leftover is removed at the next start.
- Headless, set `OP_SERVICE_ACCOUNT_TOKEN` where the server starts. At start
  the server removes it and every other `OP_*` variable, except `op signin`
  sessions, from its own environment, and passes the token to `op` only. With
  no token and no session, calls fail as locked. Nothing falls back to the
  Keychain or to files.
- Enabling 1Password adds one `op item list` to every `delete`. When `op`
  answers that the vault "isn't a vault", which it also says for a vault the
  current identity cannot see, the `delete` fails with `vault_not_visible`
  and the key's index row stays; a `get` treats the same answer as a miss.
  The index does not record which backend holds a key, so this also refuses
  deletes of Keychain or `file` keys while 1Password is enabled and the
  project has no 1Password vault. The error names the two ways out: create
  the vault in 1Password, or remove the `secrets.onepassword` section (and
  any `secrets.default_backend: onepassword`) from the machine config.
- `doctor` lists the backend without running `op`, and reports whether a
  token was present at server start (see Doctor above).

```bash
cargo install trusty-secrets --version <version> --features cli-backends --locked
```

## Keeper

Shims only, provisional: no test has run against a real Keeper account, so
the command output shapes and messages below are not yet confirmed
([#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519)).

With the `cli-backends` feature (Unix), the `keeper` backend keeps values in
Keeper through Keeper Commander, `keeper`. Only the machine config can enable
it, and both settings are required:

```yaml
secrets:
  keeper:
    program: /usr/local/bin/keeper            # `keeper`, an absolute path
    config_path: /Users/me/.keeper/config.json  # Commander's config, absolute, mode 0600
```

There is no `PATH` search, and Commander's own config search is never used.
`account` is refused: the account is the one the config file logs in to.

- Headless use needs one human step first: log in with that config file,
  approve the device, and run `this-device register` and
  `this-device persistent-login on`. Until then, and after an idle timeout,
  calls fail with `backend_locked`; the server never prompts. `doctor` does
  not detect device approval or persistent login: an available row says only
  that the server opens the backend.
- The trusty vault name is the Keeper folder path, for example
  `trusty/acme/web`. Create that folder first.
- A key is a `login` record titled with the key. Its `password` field holds
  the value. A record of another type with that title is never touched.
- The value reaches `keeper` only on stdin, as `$BASE64:` inside a
  `record-add` or `record-update` batch line (`keeper --batch-mode -`). It is
  never in argv or the environment.
- Every write is read back, and every delete is checked with a new listing;
  anything else fails. `rm` moves a record to Keeper's trash, which counts as
  deleted.
- A key is reported missing only when a listing succeeded and did not show it.
- Enabling Keeper adds a folder listing and a check listing to every `delete`.

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
- Each indexed key carries an "agents may use" flag, off by default. The
  backend holds it as its own item per key: on the Keychain, service
  `trusty-secrets.agents`, account `<vault>/<key>`, present when the flag is
  on. The names-only index never holds it, so editing the index file cannot
  turn it on. A backend with no flag item (file, 1Password, Keeper) reads every
  key off and refuses to turn one on. `store::resolve_reference` with
  `agent_parent` set refuses a key whose flag is off, or whose flag cannot be
  read, before it reads the value.

## Licence

MIT.
