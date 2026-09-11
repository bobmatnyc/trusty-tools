---
name: tm-secrets
description: Operate tm secrets — 1Password / Keeper / macOS Keychain behind trusty-common, reference-key access, and exec-wrapper recipes for gh and other CLI integrations
user-invocable: true
version: "1.0.0"
category: pm-reference
tags: [secrets, credentials, 1password, keeper, keychain, exec, pm-recommended]
effort: medium
---

# tm secrets — Credential Vault Operations

Full design: [DOC-74](../../docs/specs/DOC-74-secrets-integration.md). This
skill is the operator/PM-facing summary — read DOC-74 itself for anything not
covered here.

## When to Use This

A task touches secrets the moment it needs an API key, a login token, or any
value from a `.env`/`.env.local` file — logging in to `gh`, calling an
external API, or wiring a credential into an agent's environment. Load this
skill, or delegate to the `secrets-manager` agent, before reading a `.env`
file, before asking the user to paste a token into the transcript, and before
writing a value into an issue, PR body, commit message, or log line.

The bundled `secrets-manager` agent operates `tm secrets` on the PM's behalf.
Prefer delegating to it over running these commands yourself.

## The Store Model

Three backends, one per project:

| Backend | What it is |
|---|---|
| `keychain` | The OS's own encrypted store (macOS Keychain today). Zero CLI install. |
| `onepassword` | The 1Password CLI (`op`), an external tool the operator installs. |
| `keeper` | Keeper Commander CLI, an external tool the operator installs. |

**Default: `keychain`.** A project with no explicit choice uses the machine's
own encrypted store — no external CLI, no extra install. Set a project
override in that project's `tm` config to switch to `onepassword` or
`keeper`. Precedence: project config, when present, wins outright; otherwise
the machine default; otherwise `keychain`.

Everything lands in a per-project namespaced vault. Nothing crosses project
boundaries automatically — see `tm secrets copy` for the one sanctioned way
to move values, and note its scope (below) is a project's own vault between
backends, not vault-to-vault between two different projects.

## Command Grammar

`tm secrets` does not exist in this checkout yet — every verb below lands
with **#7521**, except `exec`, which lands with **#7525**. Nothing here is
runnable today; this table is what the shipped CLI will expose, so the skill
is ready the day the CLI lands.

| Command | Purpose |
|---|---|
| `tm secrets configure` | Detect available backends, choose the project's backend, write the config. |
| `tm secrets import [--from .env.local] [--project\|--machine]` | Bulk-load `KEY=VALUE` pairs from a dotenv file into the active vault. |
| `tm secrets add KEY [--value -]` | Add or update one key; value via stdin or a masked prompt, never argv. Also invocable as `!tm secrets add` from a session prompt. |
| `tm secrets list` | List key NAMES only, for the active project's vault — never values. |
| `tm secrets remove KEY` | Remove one key from the active vault. |
| `tm secrets copy --from <backend> --to <backend> [KEY...]` | Move named keys (or the whole vault, if none named) between backends, for the active project. |
| `tm secrets doctor` | Run backend detection, render the table, flag a configured-but-unreachable backend. |
| `tm secrets exec [--env NAME=KEY]... [--stdin KEY] -- <command...>` | Resolve each named key and inject its value into the child process's env or stdin only — never into the command's own argv. Lands with #7525. |

Reference-key MCP tools (`secrets_get_ref`, `secrets_list`) land with
**#7522** — the daemon's session-start preload and reference-key surface for
`tm`, `trusty-code`, and `trusty-agents`. Both return names and metadata
only, never a value; no MCP tool resolves an actual value into a subprocess
env, by design (DOC-74 §10.2).

## `tm secrets exec` Is the Only Way to Hand a Secret to a Command

```
tm secrets exec --stdin GH_TOKEN -- gh auth login --with-token
tm secrets exec --env GH_TOKEN=GH_TOKEN -- gh pr list
```

This is the only sanctioned path from an agent's shell call to a real secret
value. It resolves each named key from the active vault and places the value
in the child process's environment or stdin — never in the command's own
argv, never echoed by `tm` itself, never in the resulting stdout/stderr
without redaction. A daemon-side `secret://<project>/<KEY>` resolver exists
too (also #7525), for non-CLI callers building a subprocess env map
programmatically — same guarantee, different call site.

Never substitute for this with `export KEY=$(tm secrets ...)` or any shell
construct that puts a value in an env var your own turn can see or echo.

## Copying Between Stores

```
tm secrets copy --from keychain --to onepassword
tm secrets copy --from onepassword --to keeper GH_TOKEN OPENAI_API_KEY
```

Moves the named keys — or the whole vault, with none named — from one
backend to another, for the active project only. Each value is read
in-process and written to the destination backend; it is never printed.
Copying between two *different* projects' vaults is out of scope until the
owner says otherwise (DOC-74 §13.6).

## Importing From a `.env` File

```
tm secrets import --from .env.local --project
```

Loads every `KEY=VALUE` pair into the active vault. **The source file is
kept.** Import never deletes `.env`/`.env.local` after loading it — DOC-74
leaves file deletion as an open question for the owner (§13.2), and nothing
in this skill or the CLI should delete it on your own initiative. Treat the
file the way the existing convention already does: git-ignored, left in
place.

## Headless / No Interactive Unlock Available

DOC-74 does not settle this. Whether `tm secrets doctor` should fail loudly
in a detected-CI environment when the configured backend has no headless
credential present, versus silently degrading to an empty cache, is an open
owner question (DOC-74 §13.4, tracked under epic
[#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517)). Do not
invent a policy here — surface the question to the owner via the epic rather
than guessing at a default.

## Never

- Never print a resolved secret value — to the transcript, a log line, a hook
  payload, or a tool result.
- Never `cat`, `sed`, `grep`, or `echo` a `.env`/`.env.local`/`*credentials*`/
  `*secrets*` file to read its contents. The pm-guard secret-file rule
  (#7266) refuses this for every agent; `tm secrets import`/`add`/`exec`
  read such files only inside `tm`'s own Rust process, which the guard does
  not and cannot intercept.
- Never paste a secret value into an issue, a PR body, a commit message, or a
  code comment.
- Never put a resolved value in a command's argv (`some-cli --token abc123`)
  — use `tm secrets exec`'s `--env`/`--stdin` injection instead.
- Never ask the user to paste a raw secret value into the chat when a store
  already holds it — resolve it by reference instead.
