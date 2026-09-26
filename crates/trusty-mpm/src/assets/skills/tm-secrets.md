---
name: tm-secrets
description: Where a trusty credential lives today — the 0600 file store and Keychain behind trusty-common, the two tm doctor rows that check them, and the rules for handling a secret value. The tm secrets CLI does not ship yet.
user-invocable: true
version: "1.2.0"
category: pm-reference
tags: [secrets, credentials, keychain, doctor, pm-recommended]
effort: medium
---

# tm secrets — Credential Handling

🔴 **`tm secrets` does not ship.** There is no `tm secrets` subcommand in this
binary, and no `secrets_get_ref` / `secrets_list` MCP tool. Run `tm --help` to
confirm before you plan around either. The CLI is designed in
[DOC-74](../../docs/specs/DOC-74-secrets-integration.md) and tracked under epic
[#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) —
[#7521](https://github.com/bobmatnyc/trusty-tools/issues/7521) for the verbs,
[#7525](https://github.com/bobmatnyc/trusty-tools/issues/7525) for `exec`.
Neither has landed. Anything below that says "does not ship" is a statement
about this binary, not a preview to work from.

What DOES ship is the credential store inside `trusty-common`, the resolver
every daemon reader now goes through, and two `tm doctor` rows over them.

## When to Use This

A task touches secrets the moment it needs an API key, a login token, or any
value from a `.env`/`.env.local` file. Load this skill before reading a `.env`
file, before asking the user to paste a token into the transcript, and before
writing a value into an issue, PR body, commit message, or log line.

## Where a Credential Lives

| Tier | What it is |
|---|---|
| Process environment | The canonical variable for the provider (`OPENROUTER_API_KEY`, `TELEGRAM_BOT_TOKEN`, …), from `credential_registry::REGISTRY`. |
| `.env.local` | Git-ignored, loaded once per process. Never overrides an already-set variable. |
| Credential store | A **mode-0600 TOML file** under the user's home, or the macOS Keychain when the `keyring-store` feature is built in AND the backend probes available. |

**The default store is the 0600 file, not the Keychain** (#4570 owner ruling).
`trusty-common`'s default feature set excludes `keyring-store`, so a default
build resolves against the file. Do not describe the Keychain as the default,
and do not assume a value you put in the Keychain is readable by a default
build.

**`.env` is not a tier.** It is a committed, non-git-ignored file, and a
credential in one is the same defect as a credential in a LaunchAgent plist
(#8236). Readers that used to fall back to `.env` no longer do. An operator
relying on it moves the value to `.env.local` or the store.

## Reading a Credential From Rust

One function, `trusty_common::credentials::resolve_env_var_bounded`, applied
by `trusty_mpm::secret_source::resolve_secret`. Nothing else in the daemon
reads a credential variable directly.

It is **fail-closed and bounded**: every failure returns an error naming the
variable and a value-free error KIND, the dependent feature stays disabled,
and no arm falls back to a default or a stale value. The read is bounded at 3
seconds with the store read left running, because under launchd a rebuilt
binary's first Keychain read blocks on a SecurityAgent approval dialog that
only a human can clear.

## The Two `tm doctor` Rows

| Row | What it answers |
|---|---|
| `launchd_secrets` | Does an installed `com.trusty.*` LaunchAgent plist hold a credential VALUE in plaintext? Fails on a credential-shaped entry, and fails on a binary plist it cannot read. Names keys, never values. |
| `credential_reach` | Can the daemon's resolver actually reach each credential it consumes — present, absent, the store's error kind, or timed out waiting on an approval dialog? |

`tm doctor --fix` migrates a mapped plist credential into the store, confirms
a byte-equal read-back, and only then removes the plist entry. It refuses a
key with no registry mapping (removing it would disable a working feature) and
refuses a binary plist — convert that one with `plutil -convert xml1 <path>`
and re-run.

**A `credential_reach` timeout right after `cargo install` is the expected
state**, not a fault: the rebuilt binary has a new cdhash, so its first
Keychain read raises an approval dialog. Approve it once from a terminal.

## Handing a Secret to a Subprocess

There is no supported mechanism yet. `tm secrets exec` (#7525) is the design,
and it does not ship. Until it does, a credential reaches a child process the
same way it reaches the daemon — through the process environment the operator
set, or through the store. Do not invent a substitute: `export
KEY=$(something)` puts the value in a variable your own turn can echo, and a
value in argv is visible in `ps` to every process on the host.

## Never

- Never print a resolved secret value — to the transcript, a log line, a hook
  payload, a doctor row, or a tool result.
- Never `cat`, `sed`, `grep`, or `echo` a `.env`/`.env.local`/`*credentials*`/
  `*secrets*` file to read its contents. The pm-guard secret-file rule (#7266)
  refuses this for every agent.
- Never paste a secret value into an issue, a PR body, a commit message, or a
  code comment.
- Never let a credential CLI print into tool output. Check a Keychain item by
  exit status (`security find-generic-password -s <svc> >/dev/null 2>&1`, no
  `-w`/`-g`), and consume a token inside the command that needs it
  (`$(gcloud auth print-access-token)`), never as a bare run. The pm-guard
  refuses the printing forms for every agent (#8596, #8248).
- Never put a value in a command's argv (`some-cli --token abc123`).
- Never put a credential in a LaunchAgent plist, a committed `.env`, or any
  other file a `plutil -p` or a backup will print. That is #8236.
- Never ask the user to paste a raw secret value into the chat when a store
  already holds it.
