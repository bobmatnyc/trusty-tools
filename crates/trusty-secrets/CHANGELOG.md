# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.1.0] — 2026-10-06

### Added

- In-process `secret://` resolution for `tm secrets exec` ([#7525](https://github.com/bobmatnyc/trusty-tools/issues/7525), DOC-74 §9.4, §15.8):
  - `store::resolve_reference` resolves one reference: an unscoped `secret://KEY` tries the project vault, then the owner vault; `secret://<owner>/KEY` and `secret://<owner>/<repo>/KEY` read only the vault they name. A miss is `SecretsError::NotFound`.
  - With `agent_parent = true`, a key not flagged "agents may use" is refused with `SecretsError::AgentUseRefused`, before the backend is read.
  - `store::resolve_env` resolves an ordered `NAME=raw` env map all or nothing. It validates POSIX names and parses every reference before any read, and passes non-reference values through. Any bad entry fails the whole call with `InvalidEnvEntry` or `EnvResolution`, naming the entry and the reference, never a value.
  - `store::parse_dotenv` reads a documented `.env` subset (`KEY=value`, `export`, `#` comments, single and double quotes). It rejects multi-line values, escapes, `$` expansion and duplicate names with `SecretsError::DotenvSyntax`, naming the line number, never the line.
  - On the `secrets.*` socket these errors map to fixed-text kinds with no fields: `agent_use_refused` (`-32065`, also for a refusal wrapped in `EnvResolution`), `invalid_env_entry` (`-32066`), `env_resolution_failed` (`-32067`) and `dotenv_syntax` (`-32068`).
- New crate `trusty-secrets` (S1 of DOC-74 §15, [#9064](https://github.com/bobmatnyc/trusty-tools/issues/9064)): project- and owner-scoped secret storage with no dependency on trusty-mpm, trusty-console, or trusty-common.
  - `api` feature: validated `SecretKey`, `OwnerName`, `RepoName`, `VaultName` and `BackendId`; the `secret://KEY`, `secret://<owner>/KEY` and `secret://<owner>/<repo>/KEY` reference grammar; a redacting `SecretValue`; `secrets.*` request and response types; and `SecretsError`. Invalid names fail closed, and errors never echo rejected input.
  - `store` feature: the `SecretBackend` trait with `READ`, `WRITE`, `LIST_NAMES` and `SYNC_TARGET` capability flags; an OS Keychain backend over `keyring` (service = vault, account = key, no read cache); a names-only index at `~/.trusty-tools/trusty-secrets/index/` (0600 files, 0700 directory, atomic writes under a file lock, corrupt files fail closed; `set` writes the backend inside the index lock and, if the index write then fails, deletes a new entry again or reports `SecretsError::OrphanedBackendEntry` naming the vault and key); project-over-owner scope resolution from the git remote; `mask_secret`; and machine/project config resolution.
  - Platforms: the Keychain backend links `keyring` on macOS only, with only its `apple-native` backend, so no Linux build pulls libdbus. On any other target every Keychain operation fails closed with `SecretsError::UnknownBackend` instead of writing to `keyring`'s in-memory mock store.
- `trusty-secrets serve` and the default `server` feature (S2 of DOC-74 §15, [#9065](https://github.com/bobmatnyc/trusty-tools/issues/9065)): the `secrets.scopes`, `secrets.list`, `secrets.set`, `secrets.delete`, `secrets.copy` and `secrets.doctor` methods as newline-terminated JSON-RPC 2.0 on `~/.trusty-tools/trusty-secrets/secrets.sock` (parent 0700, socket 0600, every connection uid-checked).
  - The socket starts on the first call through `OnDemandSecrets` and exits after 60 s with no answered request (`TRUSTY_SECRETS_IDLE_TIMEOUT_SECS`), removing its socket file. It has no launchd job, and a second instance never replaces a live one.
  - Each request names its project by directory. The server derives owner and repository from that checkout's git remote and reads the project's `secrets:` config from `<repo>/.trusty-tools/trusty-secrets.yaml`.
  - No method returns a secret value. Every failure is fixed text per method and error kind, so a malformed `set` never echoes its value.
  - An `api`- or `store`-only consumer depends with `default-features = false` and links no tokio or UDS code.
  - A `set` whose index write and cleanup delete both fail after the backend accepted the key reports kind `orphaned_backend_entry`, code `-32064`: the backend may hold an entry that no index row lists. The error carries no key, vault or value.
  - `secrets.copy` writes each key through `SecretStore::set`, so it takes the index lock and deletes a new destination entry again when the index publish fails; an entry that cleanup cannot delete aborts the copy with `orphaned_backend_entry` instead of landing in `failed`.

### Changed

- The `secrets.*` request types `ListRequest`, `SetRequest`, `DeleteRequest` and `CopyRequest` are `#[non_exhaustive]` and gain constructors: `ListRequest::new(vault)`, `SetRequest::new(vault, key, value)`, `DeleteRequest::new(vault, key)`, and `CopyRequest::new(from_backend, to_backend)` with `.with_keys(keys)` ([#9073](https://github.com/bobmatnyc/trusty-tools/issues/9073)). Build a request with its constructor; a struct literal no longer compiles outside the crate. The wire format is unchanged.
- `ErrorKind`, `ClientError`, `SettingsError`, `ServeError`, `ScopeKind`, `BackendStatus`, `DoctorResponse` and the server `State` are `#[non_exhaustive]`, so later error kinds, doctor fields and server state are additive. A `match` on one of these enums outside the crate needs a wildcard arm; build `State` with `State::new`.
- The response types `KeyMeta`, `ScopeInfo`, `ScopesResponse`, `ListResponse`, `SetResponse`, `DeleteResponse` and `CopyResponse`, the config types `MachineSecretsConfig`, `ProjectSecretsConfig` and `ResolvedConfig`, and the enums `SetOutcome`, `SecretRef` and `VarSource` are `#[non_exhaustive]`. Read a response through serde; build a config from `Default` and assign its fields.
- The crate docs gain a "Compatibility" section: adding a field, a variant or a provided trait method is a compatible change and ships as a 0.x patch release; every request denies unknown fields, so a new request field is optional with a serde default and a client must not send it to an older server; a new response field also takes a serde default.
- The server's method table, method bodies, `build_router` and `ErrorKind::to_rpc` are no longer public; the `server` module keeps its re-exports.
- No public signature names a trusty-common type. `ClientError::Rpc` carries the new `RpcFailure` (`code`, `message`, `kind: Option<ErrorKind>`); `ClientError::Spawn`, `ClientError::Transport` and the `ServeError` sources are opaque boxed errors; `serve` returns this crate's own `ServeExit`. `Display` text is unchanged.
- `ServerSettings` and `ResolvedVar` are `#[non_exhaustive]` ([#9328](https://github.com/bobmatnyc/trusty-tools/issues/9328)); build settings with the new `ServerSettings::new(socket, index_root, machine_config, idle_timeout)`. `ResolvedConfig` implements `Default` (the Keychain backend, no vault override), so other crates can build one.
- The `keychain_roundtrip` test target requires the `store` feature, so `cargo test --no-default-features --features api` compiles.

### Security

- Files inside a repository can no longer select another project's vault ([#9328](https://github.com/bobmatnyc/trusty-tools/issues/9328), owner ruling 06). A pinned `secret://<owner>[/<repo>]/KEY` must name the caller's own project or owner vault, else `VaultOutOfScope`; a tracked `.trusty-tools/trusty-secrets.yaml` `secrets.vault` must be `trusty/<owner>/<name>` under the remote's owner, and a wider override is honoured only from the machine config's new `secrets.project_vaults` map; an `origin` remote that is not a github.com `https://`, `ssh://` (or `git+ssh://` / `ssh+git://`) or scp-form URL — including github.com over `http://`, `git://`, `file://` or a `<helper>::` prefix — is refused with the new `UnsupportedRemoteHost` error (`remote_host_unsupported` on the wire). `ScopeSet::derive` and `ScopeSet::from_identity` now take the override's source, and `parse_remote_identity` returns a `RemoteRefusal`; `RemoteRefusal` and `VaultOverride` are `#[non_exhaustive]`.

