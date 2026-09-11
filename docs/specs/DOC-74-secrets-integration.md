---
spec_refs:
  - id: SPEC-CREDAUTH-11~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-11~draft
  - id: SPEC-CREDAUTH-08~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-08~draft
  - id: SPEC-CREDPANEL-01~draft
    path: docs/specs/DOC-64-credentials-panel.md
    anchor: SPEC-CREDPANEL-01~draft
---

# DOC-74 — Secrets Integration: External Vaults (1Password, Keeper) and the OS Keychain Behind `tm secrets`

**Status:** Draft
**Spec ID:** `SPEC-SECRETS-01~draft` … `SPEC-SECRETS-14~draft` (DOC-74)
**Subsystem:** `trusty-common` — the `secrets` module (backend trait, project vault, session cache, subprocess-env resolution); `trusty-mpm` — the `tm secrets` CLI group, the `secrets_get_ref` / `secrets_list` MCP tools, the `SessionStart` preload hook, `!tm secrets add` slash entry; `trusty-agents`, `trusty-code` — consumers that resolve a `secret://` reference the same way `trusty-common::credentials` consumers do today.
**Owner:** Engineering (trusty-common) / Bob Matsuoka
**Last-updated:** 2026-09-11
**DOC-N claim:** `DOC-74`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free: `docs/specs/README.md`'s own catalog note (line 97, "Next free `DOC-N` = `DOC-74`", recorded 2026-09-02 after `DOC-73` was claimed) is current — no file under `docs/specs/**` claims `DOC-74` by filename or self-label, and no currently open PR (#7511, #7507, #7506, #7396) is a spec.
**Builds on:** [DOC-45](./DOC-45-credential-authority-model.md) — the authority (principal, `CredentialRef`, `Secret<T>`, default-deny, audit, delivery, at-rest storage). [DOC-64](./DOC-64-credentials-panel.md) — the per-assistant credential-set panel, a client of the same authority. [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md) — a grant does not survive delegation.
**Related issues:** epic pending (no issue filed yet; this document is the research/design pass requested ahead of one)

---

## 1. Relationship to DOC-45 and DOC-64 — why this is a fourth document, not an edit to either

DOC-45 answers *who may read which credential* (principal, scope, grant, audit).
DOC-64 answers *how a user manages one assistant's credential set in a GUI panel*.
Neither answers *where the encrypted bytes physically live* beyond "keyring by
default, `0600` file as the fallback" (DOC-45 §11) — a single-tier, single-vault,
single-machine model with one `KeyStore` per process.

This document is about a different axis: **project-scoped, multi-backend,
externally-managed vaults** — 1Password, Keeper, or the OS keychain, chosen
*per project*, holding a *namespace* of variables imported from `.env` files,
loaded once per session, and reachable by every trusty-* consumer (`tm`,
`trusty-code`, `trusty-agents`) through the existing `trusty_common::credentials`
resolution surface rather than a parallel one. It extends the `KeyStore` family
DOC-45 already defines (§9 of this document); it does not renegotiate DOC-45's
principal/grant/audit model, and any consumer that needs an ACL check on a
resolved value still goes through `authority::resolve` (DOC-45 §5) once that
lands — this document's `SessionSecretCache` sits *behind* that call, not
around it.

**Non-goals carried over from DOC-45:** this document grants no new reach, adds
no principal kind, and does not change who may call `resolve`. It is scoped to
*storage backend selection, import, and session-scoped delivery* only.

---

## 2. Problem Statement

Verified on `origin/main` (`edbb9dd9`):

| Claim | Verified |
|---|---|
| The only storage tiers today are file and OS keychain, chosen automatically, never per project | `resolver::default_store()` (`crates/trusty-common/src/credentials/resolver.rs:91-103`) probes `KeyringStore` then falls back to `FileKeyStore`; there is no project dimension and no external-vault backend. |
| `KeyringStore` compiles only behind a feature nobody turns on by default | `crates/trusty-common/src/credentials/mod.rs:74` gates `keyring_store` behind `#[cfg(feature = "keyring-store")]`; DOC-45 §1.3 already recorded that `default = []` leaves it uncompiled workspace-wide as of that document's writing. |
| Every credential is a single flat value, not a namespaced set of project variables | `KeyStore::get/set/unset` (`mod.rs:160-173`) take one `provider: &str` key; there is no concept of "every variable a project needs," and no bulk import path from a `.env` file into the store. |
| `.env.local` is read into the **process environment**, not into a project vault | `dotenv::load_env_local_once` (cited by `resolver.rs:7-22`) populates `std::env`, which is inherited by every child process and readable by anything in-process — the opposite of "loaded once, referenced by key, value never exposed." |
| No consumer can name "GitHub CLI login" as a subprocess-env injection without the value passing through argv or a shell string the caller composed | `GhCommand::env` (`crates/trusty-common/src/gh.rs:265-282`) takes a caller-supplied `value: impl AsRef<OsStr>` — the caller already had the plaintext in hand to pass it. There is no "resolve this reference and inject the value into the child's env without the caller ever holding it" path. |
| Subprocess output redaction against a **known secret set** already exists and works | `crates/trusty-agents/src/api/server/task_runner.rs:94,249,299` resolves `trusty_common::credentials::resolved_secret_values()` once per spawn and passes every captured line through `trusty_common::credentials::scrub_secrets` before it reaches a chat bubble or `tasks.json` — the exact mechanism this document's `tm secrets exec` needs, already proven for a different set of secrets. |
| Reading a secret-shaped file's bytes is already denied by a Bash/Read/Grep guard, independent of verb | `crates/trusty-mpm/src/bin/tm/commands/pm_guard_secret_read.rs:1-30` (issue #7266) denies any Bash segment or `Read`/`Grep` call naming a file matching `*credentials*`/`*secrets*`/`token*`/`.env`/`.env.*`/etc., regardless of the verb used on it. A `tm secrets` implementation must not create a *second*, weaker path to the same bytes (§8.4). |
| There is no per-project config surface distinct from the machine-wide `~/.trusty-tools/<crate>/config.yaml` convention | `crates/trusty-common/src/crate_config.rs:1-30` resolves exactly one config file per crate, under `$HOME`, with no per-project override. `tm secrets` needs both a machine default and a project override (§6, owner amendment 2026-09-11). |

**The gap:** an operator with 1Password or Keeper Commander installed cannot
point a trusty-tools project at it; every project on a machine shares whatever
`default_store()` picks; there is no bulk import from `.env`/`.env.local`; and
nothing scrubs a resolved value out of a subprocess's own stdout/stderr except
the one hand-rolled case in `trusty-agents`' task runner.

---

## 3. Goals

1. **G-1** One trusty-common module (`credentials::secrets` or a sibling
   `secrets` module — placement decided in §9.1) that every consumer (`tm`,
   `trusty-code`, `trusty-agents`) reaches through the existing
   `trusty_common::credentials` re-export surface, per this repo's
   common-entry-point rule.
2. **G-2** A project picks its own backend — 1Password, Keeper, or the OS
   keychain — independently of every other project on the machine; a project
   with no explicit choice uses the **machine's OS keychain** (owner ruling,
   verbatim: *"By default the store should be the machine's encrypted
   store"*).
3. **G-3** Values move between backends on command (`tm secrets copy`),
   never printed in the process.
4. **G-4** A project's variables live in one **namespaced vault** per backend
   (`trusty/<owner>/<repo>` — §6.3), importable in bulk from `.env`/`.env.local`,
   addable one at a time (`tm secrets add KEY`, and `!tm secrets add` from a
   session prompt).
5. **G-5** A session unlocks its backend **at most once** — biometric/password
   prompts do not repeat mid-session.
6. **G-6** A resolved value reaches a subprocess's environment or stdin, or an
   in-process HTTP client, and **never** reaches: the LLM context, a hook
   payload, a `tm` log line, argv visible to `ps`, or captured child
   output that gets echoed back to a caller.
7. **G-7** `tm secrets doctor` / first `configure` reports, without unlocking
   anything, which secret-management tools are actually present on the
   machine, so a user is offered real choices instead of guessing.

## 3.1 Non-Goals

- Renegotiating DOC-45's principal/grant/ACL model (§1).
- A GUI panel (DOC-64 already owns the panel surface; this document is CLI +
  daemon + trusty-common only. A future panel would be a client of both).
- Shipping a new encryption-at-rest format — 1Password/Keeper own their own
  encryption; the keychain path reuses `KeyringStore` (DOC-45 §11) unchanged.
- Rotating or generating secrets. This document only stores, retrieves, and
  moves values a human already has.
- Deleting or managing the source `.env` files beyond the open question in
  §13.

---

## 4. Trust Model

**T-1. A raw secret value is created in exactly two places:** inside the
external vault process (`op`, `keeper`, the OS keychain daemon) or inside the
trusty-common process that just asked one of them for it. It is never
constructed by string concatenation, never round-tripped through JSON that a
tool-call result serializes, and never assigned to a `String` field that
outlives the call that resolved it — the same discipline `Secret<T>`
(`crates/trusty-common/src/credentials/secret.rs:71`) already enforces for the
existing single-value credentials, extended to cover an entire project's
namespace (§9.3, `SessionSecretCache`).

**T-2. The LLM never sees a value, only a reference.** A config row, a tool
schema, and a chat transcript may all hold `secret://<project>/<KEY>` freely —
it is exactly as non-secret as `CredentialRef` is today (`handle.rs`, DOC-45
§4). The MCP tool this document adds (`secrets_get_ref`, §10.3) returns
references and metadata (last-import time, backend, whether a value is
present) — **never** a value — mirroring DOC-64's "never displays a secret
value" rule for the panel.

**T-3. Redaction points**, all reusing or extending existing machinery:

| Point | Mechanism | Precedent |
|---|---|---|
| Subprocess stdout/stderr captured and echoed to a caller (`tm secrets exec`, §10.5) | `scrub_secrets` against every value the session cache holds, applied line-by-line **before** truncation | `task_runner.rs:249,299` — proven pattern, extended to the session's full secret set |
| A hook's `tool_input`/`tool_output` payload | The `PreToolUse`/`PostToolUse` hook body is JSON the daemon logs and may forward; it is scrubbed with the same `scrub_secrets` call before it is written anywhere, and `tm secrets exec`'s injected values are never placed in `tool_input.command` text in the first place (§10.5 — env/stdin only) | New; extends the redaction discipline `pm_guard_secret_read.rs` already applies to file reads |
| `Debug`/`Display` of any in-memory type holding a value | Every type in this module implements `Debug` the way `Secret<T>` and `KeyringStore` do: either a constant redacted string or (for `KeyringStore`, stateless) nothing to leak | `secret.rs:105-126`, `keyring_store.rs:48` (`derive(Debug)` is safe only because the struct is stateless) |
| A `tm` log line | Never logs a resolved value; logs only the reference and the backend name | Same posture as `resolve_key`'s existing "never logs the (absent) value" contract (`mod.rs:163`) |
| argv of a spawned process | `tm secrets exec` never places a resolved value in argv — only in the child's environment (`--env NAME=KEY`) or its stdin (`--stdin KEY`) | New; see §10.5 and the negative test in §12 |
| A file on disk | The session cache is in-memory only (§9.3); nothing this document adds writes a decrypted value to disk. `FileKeyStore`'s existing `0600` file remains a distinct, pre-existing tier (DOC-45 §11), untouched by this document except as a migration source (§11) |

**T-4. Reading the backing store's own files/sockets directly is still
guarded.** `pm_guard_secret_read.rs` already denies naming `.env`, `.env.*`,
or any `*credentials*`/`*secrets*` path in a Bash/Read/Grep call. `tm secrets`
must not become a documented bypass of that guard: its `import`/`add`/`exec`
verbs read such files only through `dotenvy`-style parsing *inside the `tm`
binary's own Rust code* (which the guard does not and cannot intercept — it
gates the agent's tool calls, not `tm`'s own process), never by shelling out
to `cat`/`grep` on them. This mirrors how `resolver::resolve_key` already
reads `.env.local` today (`dotenv.rs`) without ever exposing its path to a
Bash call.

**T-5. Honesty clause, matching DOC-45's own.** Until DOC-45 §5's grant check
(`authority::resolve`, #4566) lands, any code that can call this module's
resolution functions gets any value in the session cache, exactly as today's
`resolve_key` gives any caller any value in the flat store (DOC-45 §1.3,
`authority.rs:37-46`). This document does not claim otherwise, and does not
block on #4566 — it is additive to the existing honesty gap, not a
regression of it.

---

## 5. Terminology

| Term | Meaning |
|---|---|
| **Backend** | An implementation that can list, read, and write a namespaced set of key/value pairs: `onepassword`, `keeper`, or `keychain`. |
| **Project vault** | The namespaced collection of variables one project stores in one backend — `trusty/<owner>/<repo>` (§6.3). |
| **Reference key** | `secret://<project>/<KEY>`, or the shorthand `${tm:KEY}` inside the active project's own scope — non-secret, freely printable (§4 T-2, open question in §13). |
| **Session cache** | The in-process, per-`tm`-session store of resolved values for the active project's vault, populated once at session start or first use (§9.3). |
| **Detection** | A read-only probe of what secrets tooling exists on the machine — installed, running, configured — never an unlock (§7). |

---

## 6. Configuration

### 6.1 Two levels, one precedence rule

**Machine level** — `~/.trusty-tools/trusty-common/config.yaml`, following the
existing `crate_config` convention (`crate_config.rs:1-30`) exactly:

```yaml
secrets:
  default_backend: keychain   # owner ruling: this is the shipped default
```

**Project level** — inside the project's own tracked config, alongside where
`trusty-tools-config`/`agents.ticketing` already lives per this repo's own
`trusty_tools_config.rs` convention (cited by `commands/issue/mod.rs:39-42`):

```yaml
secrets:
  backend: onepassword         # overrides the machine default for this project only
  vault: trusty/bobmatnyc/trusty-tools   # optional explicit override of §6.3's derived name
```

**Precedence: project config, when present, wins outright; otherwise the
machine default; otherwise `keychain`** (three-tier fallback, never a merge —
a project either names a backend or it doesn't). A project with zero
`secrets:` config and a machine with zero `secrets:` config both resolve to
`keychain` with no CLI installed and no prompt — this is what makes
`keychain` a true zero-configuration default (owner ruling, §3 G-2).

### 6.2 Backend-specific settings

```yaml
secrets:
  backend: onepassword
  onepassword:
    account: my.1password.com   # `op` account shorthand, passed to `op --account`
  keeper:
    config_path: ~/.keeper/config.json   # KSM config, when not the CLI default
```

Neither section ever holds a token or password — only the shape needed to
invoke the CLI (account name, config path). A service-account token
(`OP_SERVICE_ACCOUNT_TOKEN`, headless Keeper KSM config) is read from the
external tool's own documented environment/config location, never copied into
trusty-tools' own config file (T-1).

### 6.3 Vault naming

Default vault name is derived, never chosen ad hoc: `trusty/<owner>/<repo>`,
where `<owner>/<repo>` comes from the same git-remote-derived identity `tm`
already resolves for GitHub account selection (memory: "tm GitHub account
selection" — owner ruling that a managed session picks the right account from
the remote). A project may override it explicitly (§6.1) when it wants to
share a vault across repos or match an existing 1Password vault name.

---

## 7. Backend Detection (`detect_backends`)

**API** (trusty-common):

```rust
pub enum ToolStatus {
    Unsupported,          // trusty-tools has no adapter for this tool at all
    NotInstalled,         // adapter exists; binary/app not found
    Installed,            // binary on PATH (or app bundle present), not confirmed running/configured
    Running,              // desktop app / daemon process or its IPC socket is present
    Configured,           // account/vault config found (e.g. `op account list` has an entry, KSM config exists)
}

pub struct DetectedBackend {
    pub id: &'static str,       // "onepassword", "keeper", "keychain", "bitwarden", "vault", "pass", "gopass", "doppler", "infisical"
    pub status: ToolStatus,
    pub detail: Option<String>, // e.g. "op 2.28.0 on PATH", "~/.keeper/config.json found"
}

pub fn detect_backends() -> Vec<DetectedBackend>;
```

**What it probes, deterministically, with no unlock and no network call:**

| Tool | Installed | Running/Configured |
|---|---|---|
| 1Password CLI (`op`) | PATH lookup via the existing `bin_resolve::resolve_binary` (`crates/trusty-common/src/bin_resolve.rs:121`) — the same helper `locate_uv` already uses, reused rather than a second `which`-alike | `op account list` config file present (`~/.config/op/config`), or the desktop-app integration socket present |
| Keeper Commander / KSM (`keeper`, `ksm`) | PATH lookup, same helper | `~/.keeper/config.json` (Commander) or a KSM config file present |
| OS keychain | Always "installed" on macOS/Windows/Linux-with-Secret-Service | `KeyringStore::probe_available()` (`keyring_store.rs:64`) reused verbatim — it already does exactly this probe, cached process-wide |
| Bitwarden CLI (`bw`) | PATH lookup | listed as **detected, not yet supported** — `Unsupported` status regardless of PATH result, `detail` still reports the binary if found |
| HashiCorp Vault (`vault`) | PATH lookup | `Unsupported`, same as above |
| `pass` / `gopass` | PATH lookup | `Unsupported`, same as above |
| Doppler (`doppler`) | PATH lookup | `Unsupported`, same as above |
| Infisical (`infisical`) | PATH lookup | `Unsupported`, same as above |

Detection is pure PATH/file/socket inspection — no `op read`, no `keeper get`,
no keychain `get_password` beyond the existing sentinel-account probe that
never touches a real credential. `tm secrets doctor` renders the result as a
table; `tm secrets configure` runs the same detection first and offers only
the backends that are not `Unsupported` as choices, defaulting the prompt to
whichever already reports `Configured` (or `keychain` when none does).

---

## 8. The `trusty-common` API

### 8.1 `SecretsBackend` — extends, does not replace, `KeyStore`

```rust
/// One backend that can hold a project's whole namespaced variable set.
/// Where `KeyStore` (mod.rs:160) is "one flat provider→value table," this
/// trait is "one namespaced vault," and every `SecretsBackend` impl can be
/// adapted to `KeyStore` for a single project's namespace when an existing
/// `KeyStore` consumer needs one (adapter in §11).
pub trait SecretsBackend: Send + Sync {
    fn list(&self, vault: &ProjectVault) -> Result<Vec<String>, SecretsError>; // names only, never values — mirrors KeyStore::list's contract
    fn get(&self, vault: &ProjectVault, key: &str) -> Result<Option<Secret<String>>, SecretsError>;
    fn set(&self, vault: &ProjectVault, key: &str, value: &str) -> Result<(), SecretsError>;
    fn remove(&self, vault: &ProjectVault, key: &str) -> Result<(), SecretsError>;
    fn backend_id(&self) -> &'static str;
}
```

Three implementations: `OnePasswordBackend`, `KeeperBackend` (both subprocess-
backed, §8.2), and `KeychainBackend` (a thin namespacing adapter over the
existing `KeyringStore`, storing `"<vault>/<key>"` as the keyring account —
`KeyringStore` already has no enumeration API (`keyring_store.rs:126-132`), so
`KeychainBackend::list` keeps its own small index file, `0600`, under
`~/.trusty-tools/trusty-common/secrets-index/<vault>.json`, holding **key
names only, never values** — the one piece of local bookkeeping the keychain
itself cannot provide).

### 8.2 Subprocess delivery for `op` and `keeper`

Both external CLIs are subprocesses, so both route through the same shared
entry point `gh.rs` already established for `gh` — a sibling `OpCommand` /
`KeeperCommand` (or a generalization of `GhCommand` into a `ExternalCliCommand`
the three share) built the same way: a typed builder, `.env()`/`.env_remove()`
for the parent's env, `.to_std_command()` for a caller that needs a raw
`Command`, `.output_blocking()` / an async `.output()` for the common case.
This satisfies this repo's common-entry-point rule (CLAUDE.md: "every
capability shared across two or more crates … MUST have exactly one
implementation") for a third and fourth CLI subprocess family, rather than a
third and fourth hand-rolled `Command::new`.

```rust
// op read "op://<vault>/<item>/<field>"  →  one value, stdout only, never argv-visible on the value side
// op item create --category=login --vault <vault> --title <key> password=<value> --format=json  (via stdin, not argv — op supports assignment via - / stdin)
// keeper get <record-uid> --format json   /   ksm secret get <uid> --format json
```

**Delivery of the value out of the subprocess never touches argv.** `op read`
takes a reference in argv (not a secret), and returns the secret on stdout —
safe. Writing a *new* value (`tm secrets add`) pipes the value to the CLI's
stdin (`op item create … password=- ` / Keeper Commander's `--from-file -`)
rather than composing it into the argv this document's own `ExternalCliCommand`
would otherwise render into a log line (see `GhCommand::argv_display`,
`gh.rs:289`, which exists precisely so a command can be logged — the new
commands must never call the equivalent for a value-carrying argument).

### 8.3 `ProjectVault`

```rust
pub struct ProjectVault {
    id: String,      // "trusty/bobmatnyc/trusty-tools", derived per §6.3 or overridden
}
```

Deliberately as thin and non-secret as `CredentialRef` (`handle.rs:109-113`)
— a vault identifier is a name, not a secret, and is safe to log, print, and
put in an error message.

### 8.4 `SessionSecretCache` — single unlock per session

```rust
/// Populated once per `tm` session (daemon-side) or once per process (a bare
/// `tm secrets` CLI invocation with no daemon), from exactly one backend
/// call per key on first need — never re-probed, never re-prompted.
pub struct SessionSecretCache { /* vault -> key -> Secret<String>, process-lifetime */ }

impl SessionSecretCache {
    pub fn preload(&self, backend: &dyn SecretsBackend, vault: &ProjectVault) -> Result<usize, SecretsError>; // called at SessionStart (§10.4)
    pub fn resolve(&self, reference: &SecretRef) -> Result<Secret<String>, SecretsError>; // fills on first use if `preload` was skipped
    pub fn known_values(&self) -> Vec<String>; // for scrub_secrets — mirrors `resolved_secret_values()` (mod.rs:95) exactly, extended to this cache
}
```

Backed by a `OnceCell`-per-vault the same way `KeyringStore`'s
`PROBE_RESULT` (`keyring_store.rs:41`) is a process-wide, once-only cache —
same pattern, same rationale (§9's "populate once, never re-probe" contract),
scaled from one boolean to one map. A locked backend (denied biometric,
expired `op` session) surfaces as `SecretsError::BackendLocked` on the
*first* call in the session and is not silently retried per key — one prompt,
one outcome, for the whole session, matching the owner's "biometric/passwords
only requested once" requirement verbatim.

### 8.5 `SecretsError`

```rust
pub enum SecretsError {
    BackendUnavailable { backend: &'static str, detail: String }, // CLI not on PATH, keychain probe failed
    BackendLocked { backend: &'static str },                       // unlock declined/expired — surfaces once per session (§8.4)
    NotFound { vault: String, key: String },
    Io { detail: String },       // subprocess spawn/exit failure, never carries stdout/stderr verbatim (could hold a value)
    Parse { detail: String },    // CLI output shape changed
}
```

Every variant is `Debug`/`Display`-safe by construction — none carries a
`String` sourced from a resolved value; `Io`'s `detail` is a fixed message
plus exit code, never the child's captured output (which is exactly the
surface T-3 already scrubs before it becomes visible anywhere).

---

## 9. `tm secrets` Command Grammar

```
tm secrets configure                          # detect backends (§7), prompt for machine or project-level choice, write §6 config
tm secrets import [--from .env.local] [--project|--machine]
                                               # bulk-load KEY=VALUE pairs into the active vault; never deletes the source file (open question, §13)
tm secrets add KEY [--value -]                 # add/update one key; value via stdin (`-`) or an interactive masked prompt, never argv
tm secrets list                                # key NAMES only, per project vault — never values (mirrors KeyStore::list, mod.rs:171)
tm secrets remove KEY
tm secrets copy --from <backend> --to <backend> [KEY...]
                                               # moves the named keys (or the whole vault, if none named) from one backend to another
                                               # for the ACTIVE project; reads each value in-process and writes it to the destination
                                               # backend without ever printing it — the owner's "copy vars between stores" requirement
tm secrets doctor                              # runs detect_backends (§7), renders the table, flags a configured-but-unreachable backend
tm secrets exec [--env NAME=KEY]... [--stdin KEY] -- <command...>
                                               # resolves each named KEY from the active vault and injects the VALUE into the child's
                                               # environment (--env) or stdin (--stdin) only — never into <command...>'s own argv, never
                                               # echoed by tm itself. Example: `tm secrets exec --stdin GH_TOKEN -- gh auth login --with-token`
                                               # Example: `tm secrets exec --env GH_TOKEN=GH_TOKEN -- gh pr list`
```

`!tm secrets add` from a session prompt is the same `add` verb, routed
through the daemon's existing bang-command dispatch (the same seam
`misc.rs`'s hook handler and the CLI share a `clap` definition for — no new
parsing path); the interactive value prompt is masked in the terminal and
never appears in the turn's transcript, matching T-2/T-3.

**`tm secrets exec` is the "integrate with `gh` without exposing the
password" seam the owner asked for.** It is the CLI-level sibling of §9.5's
daemon-side `secret://` env-map resolution: the CLI form is for an
interactive or scripted shell; the daemon form is for a spawn issued
programmatically (a workflow step, an MCP tool). Both terminate in the same
`SessionSecretCache::resolve` call and the same T-3 redaction of anything the
child echoes back.

### 9.5 Daemon-side: `secret://` in a subprocess env map

The shared subprocess helper `gh.rs` extends (§8.2) generalizes to every
trusty-* consumer that spawns a child with an explicit env map — `git`, `gh`,
future `op`/`keeper` calls themselves. Any value in that map of the shape
`secret://<project>/<KEY>` is resolved through `SessionSecretCache` **at
spawn time**, substituted into the literal `Command::env()` call, and never
otherwise materialized — the caller that built the env map holds only the
reference string, satisfying the same use-time-resolution discipline DOC-45
`C-8.4` already requires of `authority::resolve` (`authority.rs:8-13`).
Redaction: the helper collects `SessionSecretCache::known_values()` before
spawning and applies `scrub_secrets` to every line of captured stdout/stderr
before it is returned to *any* caller — daemon-side workflow output included,
not just `tm secrets exec`'s — closing the same leak class `task_runner.rs`
already closed for the flat credential store.

---

## 10. Daemon Integration

### 10.1 Session-start preload

`tm hook`'s existing `event == "SessionStart"` branch (`misc.rs:547`) gains a
second one-time action alongside the #7245 savings-row emission already
there: resolve the active project (the same git-remote-derived identity
§6.3 already uses), read its `secrets.backend` (§6.1), and call
`SessionSecretCache::preload`. A locked/unavailable backend logs a single
warning naming the backend and proceeds with an empty cache — never blocks
session start, matching the resolver's existing "degrade, don't break"
posture (`resolver.rs:74-81`, `default_store`'s own fallback chain).

### 10.2 `tm secrets` and MCP: a reference-returning tool, not a value-returning one

A new tool, `secrets_get_ref`, joins the daemon's MCP catalog
(`crates/trusty-mpm/src/mcp/tools/mod.rs` — the pattern is `mod.rs`'s thin
facade plus a new `secrets.rs` leaf, matching how `disk.rs` was added for
Disk in #6927) alongside `secrets_list`. Both return only names/references
and metadata (`backend`, `imported_at`, `present: bool`) — never a value —
the same non-goal DOC-64 states for its panel. A tool that resolves an actual
*value* into a subprocess environment (the MCP equivalent of `tm secrets
exec`) is deliberately **not** exposed to the model at all: value resolution
happens only in Rust code that builds a subprocess env map or an HTTP client
(§9.5), never through a tool call whose `ToolResult` an LLM turn could echo.

### 10.3 Where session state lives

`SessionSecretCache` is per-daemon-session state, held the same place a
managed session's other per-session state lives (the session record
`session_manager` already tracks, cited at `crates/trusty-mpm/src/session_manager/hook_sync.rs:25-56`
for the analogous `SessionStart`→session-record binding) — not persisted to
disk, dropped when the session ends, and never serialized into a session
snapshot (`.trusty-mpm/sessions/*.md` files are tracked in git per this
repo's own convention — a cache entry must never reach one).

---

## 11. Consumers and Migration

- **`tm`** reaches the cache directly (in-process, same binary).
- **`trusty-agents`** and **`trusty-code`** reach it exactly the way they
  reach today's flat credential store: through `trusty_common::credentials`
  re-exports (`resolve`, `resolve_key`, and this document's new
  `secrets::resolve_from_vault` alongside them) — no new dependency edge, no
  second entry point, per this repo's common-entry-point rule and DOC-45
  `C-3.3`.
- **Migration from `FileKeyStore`.** `FileKeyStore` remains the fallback
  tier DOC-45 §11 already specifies for the flat, non-project-scoped
  credential store (inference provider keys, channel tokens); it is not
  superseded. A project that opts into `tm secrets` for *project* variables
  is layering a second, namespaced concern on top, not replacing the first —
  `resolve_key`'s 3-tier precedence (env > `.env.local` > flat store) is
  untouched. `tm secrets import` is additive: it moves rows from a
  `.env`/`.env.local` file into a project vault; it does not read or migrate
  `FileKeyStore`'s existing rows (a future `tm secrets copy --from file
  --to keychain` could, but is out of scope here — §13).

---

## 12. Test Plan

Per the workspace's [Rust Test Ladder](../../CLAUDE.md#rust-test-ladder--how-much-testing-this-change-needs),
a new cross-crate security-sensitive surface is **rung 5**: rung-4 dependent
coverage plus failure-path/concurrency tests and a `code-critic` round.

| Area | Test | Mirrors |
|---|---|---|
| `Secret`-shape safety for the whole vault cache | Compile-time `AmbiguousIfImpl` assertions that `SessionSecretCache` cannot be `Serialize`/`Clone` as a whole, and that a resolved value is only ever `Secret<String>` | `secret.rs:148-183`'s `not_serialize_not_clone` |
| Redaction completeness | Property test: for every specimen value the cache holds, `scrub_secrets` output contains no substring ≥3 chars of any of them, across resolved-value permutations | `secret.rs:267-291`, `push_stderr_tail`'s existing redaction tests (`task_runner.rs` test list) |
| `tm secrets exec` argv isolation | Spawn a child with `tm secrets exec --env FOO=BAR -- printenv`, capture the **parent's** view of `/proc/<pid>/cmdline` (or `ps -o command` on macOS) for the child, assert the resolved value is absent from it while `printenv` (reading its own env) still sees it | New — the owner's explicit "absent from argv" requirement |
| Hook payload scrub | Construct a `PreToolUse` payload whose `tool_input.command` is exactly a `tm secrets exec` invocation, assert the daemon's logged/forwarded copy of that payload never contains a resolved value (only ever the `KEY` name, since the value was never placed in `command` text — §9.5) | New — the owner's "absent from the hook's tool_input" requirement |
| Captured output scrub | Run a fixture child that deliberately echoes an injected env value to stdout via `tm secrets exec`, assert the value returned to the caller/logged is redacted | New, generalizing `stderr_tail_redacts_a_known_credential`'s pattern to this cache |
| Single-unlock-per-session | `MemoryBackend` test double that panics on a second unlock call; two `resolve` calls in the same session must not trigger it twice | New; parallels `KeyringStore`'s existing `probe_is_cached_across_instances` (`keyring_store.rs:155-160`) |
| Backend detection | Fixture `PATH` containing a fake `op`/`keeper` shim and a fake socket path, assert `detect_backends()` reports `Installed`/`Running` correctly per tool and `Unsupported` for the listed-but-unimplemented tools (`bw`, `vault`, `pass`, `gopass`, `doppler`, `infisical`) without any real network or unlock call | New, in the spirit of `keyring_store.rs`'s "probe never touches a real keychain in tests" discipline |
| Precedence: project config over machine default | `MemoryKeyStore`-style hermetic config resolution test: project config present → its backend wins; absent → machine default; both absent → `keychain` | Parallels `resolver_tests::env_beats_store` et al. (`resolver.rs:172-243`) |
| `copy` never observes the destination write partially applied | Interrupt (simulated backend error) mid-copy of N keys; assert already-written keys are reported and no value appears in the error text | New — DOC-45-style honesty about partial-failure states |
| Consumer rung-4 coverage | `cargo check --workspace`, then `cargo test -p trusty-agents --no-fail-fast` and `cargo test -p trusty-code --no-fail-fast` (or feature-scoped equivalents) once either consumer calls the new resolution path | This repo's cross-crate rung 4 |

---

## 13. Open Questions for the Owner

1. **First backend to implement.** This document specifies all three
   (`keychain`, `onepassword`, `keeper`) at the API/CLI-grammar level, but
   `keychain` is the only one with zero new subprocess surface (it reuses
   `KeyringStore` verbatim) and is already the shipped default (§6.1) —
   recommend it ships first, with `onepassword`/`keeper` as the next two
   PRs. Confirm or reorder.
2. **`.env`/`.env.local` deletion after import.** §9's `tm secrets import`
   is specified as non-destructive (never deletes the source file). Should
   it offer `--delete-after` once every imported key round-trips
   successfully, or is leaving the plaintext file in place (git-ignored, per
   existing convention) always the right default?
3. **Reference syntax.** `secret://<project>/<KEY>` (used throughout this
   document, parallel to `CredentialRef`'s `provider/qualifier` shape) versus
   `${tm:KEY}` (shorter, shell-familiar, ambiguous with actual shell
   expansion when pasted into a script). Pick one canonical form; the other
   can remain a display alias at most.
4. **Headless/CI behaviour.** `OP_SERVICE_ACCOUNT_TOKEN` and a Keeper KSM
   config both work headlessly by the external tool's own design (§7). Should
   `tm secrets doctor` in a detected-CI environment (`CI=true`) fail loudly
   when a project's configured backend has no headless credential present,
   rather than silently degrading to an empty cache (§10.1)?
5. **Cross-project vault sharing.** §6.3's default vault name is per-repo. Is
   an explicit `secrets.vault:` override (already in §6.1's example) the only
   sanctioned way to share one vault across repos, or should a monorepo-style
   `trusty/<owner>/<org-wide-name>` convention exist too?
6. **`tm secrets copy`'s scope.** Confirmed in-scope: moving a project's own
   vault contents between backends. Out of scope unless the owner says
   otherwise: copying between two *different* projects' vaults, which is a
   distinct, more sensitive operation (crosses the DOC-45 §12
   project-trust boundary) and is not requested here.

---

## 14. Summary of Owner Amendments Folded In (2026-09-11)

For traceability, every clause below responds to a verbatim owner instruction
received while this document was drafted:

- "Each project can pick its own preferred store, and also copy vars between
  stores" → §6.1 (per-project backend), §9 `tm secrets copy` (§3 G-3).
- "By default the store should be the machine's encrypted store" → §6.1's
  three-tier precedence, `keychain` as the shipped default (§3 G-2).
- "Let's make the store accessible to integration tools like gh so we can
  login without exposing pw (through bash calls)" → §9 `tm secrets exec`,
  §9.5 daemon-side `secret://` env-map resolution, §4 T-3's redaction table,
  §12's argv/hook-payload/output negative tests.
- "The secrets manager should also auto-detect any secrets management tools
  running locally" → §7 `detect_backends`, §9 `tm secrets doctor`, §12's
  detection fixture test.
