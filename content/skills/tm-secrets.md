---
name: tm-secrets
description: Where a trusty credential lives today — the trusty-common store behind the daemon, and the trusty-secrets 0.1.0 Keychain vaults and socket — plus the rules that keep a secret value out of every transcript. The tm secrets CLI does not ship yet.
user-invocable: true
metadata:
  version: "1.3.0"
category: pm-reference
tags: [secrets, credentials, keychain, doctor, pm-recommended]
effort: medium
---

# tm secrets — Credential Handling

🔴 **No agent-callable secrets client ships.** These are designed in
[DOC-74](../../docs/specs/DOC-74-secrets-integration.md) §15 and are not in
any binary yet: the `tm secrets` CLI
([#7521](https://github.com/bobmatnyc/trusty-tools/issues/7521)), `tm secrets
exec` ([#7525](https://github.com/bobmatnyc/trusty-tools/issues/7525)), the
`secrets_get_ref` MCP tool
([#7522](https://github.com/bobmatnyc/trusty-tools/issues/7522)), the console
secrets page, the file backend
([#9326](https://github.com/bobmatnyc/trusty-tools/issues/9326)), the audit
trail ([#4567](https://github.com/bobmatnyc/trusty-tools/issues/4567)),
`secrets.resolve` and exec grants
([#9070](https://github.com/bobmatnyc/trusty-tools/issues/9070)), and the
language clients ([#9071](https://github.com/bobmatnyc/trusty-tools/issues/9071),
[#9072](https://github.com/bobmatnyc/trusty-tools/issues/9072)). Run `tm --help`
before you plan around any of them.

What ships: the `trusty-common` credential store the daemon reads, and the
`trusty-secrets` 0.1.0 crate — a Keychain backend, a names-only index and an
on-demand socket.

## When to Use This

Load this skill before a task touches an API key, a login token, or any value
from a `.env`/`.env.local` file: before reading such a file, before asking the
user for a token, and before writing an issue, PR body, commit message or log
line that could carry one.

## Rules

1. Never read, echo, log or paste a secret value — into the transcript, a log
   line, a hook payload, a doctor row, a tool result, an issue, a PR body, a
   commit message or a code comment.
2. Refer to a secret by `secret://` reference or by vault and key name only.
3. Never put a value in a command's argv (`some-cli --token abc123`) or in an
   env overlay you write (`KEY=value cmd`, `env KEY=value cmd`). Hand it on
   stdin. The one exception is a credential CLI's output for a program that
   cannot read stdin (see "Handing a Secret to a Subprocess"); the expanded
   value is visible to `ps` while the command runs, so prefer a short-lived
   token. Rust code hands a value to a vendor CLI through
   `trusty_common::credentials::ExternalCliCommand`
   ([#9311](https://github.com/bobmatnyc/trusty-tools/issues/9311)), which
   refuses a value in argv or in the env overlay before it spawns.
4. Never commit or stage a `.env` or `.env.*` file. A committed credential is
   the same defect as one in a LaunchAgent plist (#8236). An example file
   holds key names and `secret://` references only.
5. Never move a value out of the Keychain — no `security … -w` export, no copy
   into a file, a plist, `.env.local` or another store.
6. Never call `secrets.set`, and never hand-roll a call to the `trusty-secrets`
   socket from a shell or a script. The `secrets.set` request carries the
   value. `tm hook --pm-guard` refuses `nc`.
7. Never `cat`, `sed`, `grep`, `Read` or `echo` a `.env`/`.env.local`/
   `*credentials*`/`*secrets*` file. The pm-guard secret-file rule (#7266)
   refuses this for every agent; do not work around it.
8. Never ask the user to paste a value into the chat.
9. Never let a credential CLI print into tool output. Check a Keychain item by
   exit status (`security find-generic-password -s <svc> >/dev/null 2>&1`, no
   `-w`/`-g`), and consume a token inside the command that needs it, never as
   a bare run (#8596, #8248).
10. Never list Vercel env vars in a form that can print a value. Filter to
    NAMES at the source: `vercel env ls <env> | awk 'NR>1{print $1}'` —
    never `--json`, and never the unfiltered table, which carries
    value-adjacent data. A value that prints anyway is an exposure to report
    (#9158).

A value that reaches a transcript, log or commit is a security defect. Stop
and report its location to the PM; it is already exposed.

## trusty-secrets 0.1.0

Project and owner secrets for the trusty-* tools. Nothing outside the crate
calls it yet.

**Vaults.** The `origin` remote names the owner and repository. It must be a
github.com `https://`, `ssh://` (or `git+ssh://`, `ssh+git://`) or scp-form
URL; any other host or scheme is refused.

| Scope | Vault |
|---|---|
| Project | `trusty/<owner>/<repo>` |
| Owner | `trusty/<owner>` |

A tracked `.trusty-tools/trusty-secrets.yaml` may override the project vault
only with `trusty/<owner>/<name>` under the remote's owner. Any wider override
lives in the operator's machine config (`secrets.project_vaults` in
`~/.trusty-tools/trusty-common/config.yaml`). Agents edit neither file to
widen scope.

**References.** `secret://KEY` checks the project vault, then the owner vault.
`secret://<owner>/KEY` and `secret://<owner>/<repo>/KEY` name a vault
explicitly, and may name only the caller's own project or owner vault.

**Backend.** The macOS Keychain: service = vault, account = key. Every read
goes to the OS; nothing is cached. On any other OS there is no store and every
call fails with `unknown_backend`; it never falls back to a file.

**Names-only index.** `~/.trusty-tools/trusty-secrets/index/` holds key names,
value lengths, update times and the "agents may use" flag (default off). It
holds no value. Do not edit it.

**Socket.** `trusty-secrets serve` answers on
`~/.trusty-tools/trusty-secrets/secrets.sock`, started by a client on first
call and gone after 60 s idle. Each request names its project directory; the
server derives the vaults from that checkout's remote.

| Method | Returns |
|---|---|
| `secrets.scopes` | The project and owner vaults |
| `secrets.list` | Name, length, update time and agents flag per key — never a value |
| `secrets.set` | `new` or `updated`, and the first 8 characters plus length once. Its request carries the value. |
| `secrets.delete` | Whether a key was removed |
| `secrets.copy` | Names copied and failed. Keychain is the only backend, so no copy has a destination. |
| `secrets.doctor` | Backend availability and paths |

Only `secrets.set` echoes any part of a value: its one-time confirmation
shows the first 8 characters and the length, or only the length for a value
of 8 characters or fewer (owner ruling 2026-10-01). That is one reason agents
never call it. No other method returns any part of a value.

## Refusals

Every refusal is fixed text with a `kind`; none carries a value. Report the
kind and the key or reference to the PM. Do not retry under another name.

| Kind | Meaning and action |
|---|---|
| `vault_out_of_scope` | The reference or vault is not the project's or its owner's. Cross-project access does not ship. Do not widen a config file. |
| `remote_host_unsupported` | `origin` is not a github.com URL in an accepted form. Do not change the remote. |
| `project_unresolved`, `project_invalid` | No usable checkout or remote. Report the directory. |
| `agent_use_refused` | The key's "agents may use" flag is off. Only the operator changes it. |
| `not_found` | Neither vault indexes the key. Tell the PM the vault and key; the operator enters the value outside every session. |
| `unknown_backend`, `same_backend` | No such backend on this build or OS, or a copy onto itself. |
| `backend_failed`, `index_busy`, `index_corrupt`, `storage_unavailable`, `orphaned_backend_entry` | Store fault. Report it; do not touch the Keychain or the index by hand. |
| Any other kind | Report it verbatim. |

## trusty-common: The Daemon's Credentials

| Tier | What it is |
|---|---|
| Process environment | The canonical variable for the provider (`OPENROUTER_API_KEY`, `TELEGRAM_BOT_TOKEN`, …), from `credential_registry::REGISTRY`. |
| `.env.local` | Git-ignored, loaded once per process. Never overrides an already-set variable. |
| Credential store | A **mode-0600 TOML file** under the user's home, or the macOS Keychain when the `keyring-store` feature is built in AND the backend probes available. |

**The default store is the 0600 file, not the Keychain** (#4570 owner ruling).
A default build does not read a value you put in the Keychain. **`.env` is not
a tier**; no reader falls back to it.

Rust reads a credential through
`trusty_common::credentials::resolve_env_var_bounded`, applied by
`trusty_mpm::secret_source::resolve_secret`. It fails closed with a value-free
error kind and is bounded at 3 seconds, because a rebuilt binary's first
Keychain read under launchd waits on an approval dialog.

| `tm doctor` row | What it answers |
|---|---|
| `launchd_secrets` | Does an installed `com.trusty.*` LaunchAgent plist hold a credential value in plaintext? Names keys, never values. |
| `credential_reach` | Can the daemon's resolver reach each credential — present, absent, the store's error kind, or timed out on an approval dialog? |

`tm doctor --fix` moves a mapped plist credential into the store, confirms a
byte-equal read-back, then removes the plist entry. It refuses an unmapped key
and a binary plist (`plutil -convert xml1 <path>`, then re-run). A
`credential_reach` timeout right after `cargo install` is expected: approve the
dialog once from a terminal.

## Handing a Secret to a Subprocess

Nothing injects a vault value into a child process yet. A credential reaches a
child the way it reaches the daemon: through the environment the operator set,
or the store. Do not invent a substitute: `export KEY=$(something)` parks the
value where your turn can echo it.

A credential CLI's output (`security … -w`, `gcloud auth print-access-token`)
is consumed inside the command that needs it: pipe it to a stdin reader
(`… | docker login --password-stdin …`), or, only when the program cannot
read stdin, `$(…)` in the one argument that needs it. The expanded value is
visible to `ps` while that command runs, so prefer a short-lived token. Never
assign it to a variable first (#8596, #8248).
