# trusty-secrets

**Status: new, version 0.1.0, published as a crate only.** The `tm secrets`
CLI and the MCP tool that use this crate ship in a later trusty-mpm release.
Nothing on this page about them is available today.

trusty-secrets stores API keys, tokens and passwords for your projects. Callers
refer to a secret by key name or `secret://` reference. The value stays in the
OS store, and no `secrets.*` method returns it.

Tracking: [#9073](https://github.com/bobmatnyc/trusty-tools/issues/9073) (publish),
epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517).
Design: [DOC-74](../specs/DOC-74-secrets-integration.md) section 15.
Product intent: [PRD-SECRETS-01](../prd/PRD-SECRETS-01-console-secrets.md).
The crate README is the entry point for usage:
[`crates/trusty-secrets/README.md`](../../crates/trusty-secrets/README.md).

## What it does

- Keeps secrets in two scopes: project (one repository) and owner (every
  project of one GitHub owner). The project value wins when both exist.
- Stores values in the macOS Keychain. On any other OS there is no value store
  and every operation fails closed. There is no file fallback for values.
- Keeps a names-only index (key names, lengths, update times) in 0600 files.
- Serves the `secrets.*` methods on an on-demand Unix socket. The
  `trusty-secrets` binary starts on the first call and exits after 60 seconds
  idle.
- Shows a value once after a write, masked: the first 8 characters and the
  length. Values of 8 characters or fewer show only the length.

## Install

```bash
cargo install trusty-secrets --version 0.1.0 --locked
```

## Features

`api` (names, references, wire types), `store` (backends, index, scopes,
masking), `server` (the socket and binary) and `test-support` (an in-memory
backend). The first three are on by default. On Unix, `api` also carries
`granted`, the client a program started by `tm secrets exec` uses to read a
granted value. The
[crate README](../../crates/trusty-secrets/README.md) lists what each adds and
has a library example.

## Sync targets

A sync target is a remote place that receives secret values and never gives
them back. The local store stays the source. A sync target is write-only,
never read back, never a configured backend, and never swept by `delete`.
Design: [DOC-74](../specs/DOC-74-secrets-integration.md) section 15.4.

On main there is one: `github-actions`. It needs a build with the non-default
`cli-backends` feature (Unix only); without it the target does not open. Build
it with `cargo install trusty-secrets --version <version> --features
cli-backends --locked`. It is not in a published release yet: the last release
is 0.1.6, and the change is unreleased.

It is provisional. No test has run against a real repository, so the `gh`
output shapes and messages are not yet confirmed
([#9069](https://github.com/bobmatnyc/trusty-tools/issues/9069)). A Vercel
target is in progress and is not described here.

Push named keys into the Actions secrets of a repository:

```bash
tm secrets copy --from <local> --to github-actions KEY_ONE KEY_TWO
```

| Rule | Behaviour |
|---|---|
| Keys | A key list is required. An empty list fails with `invalid_params`. |
| Repository | The `github.com/<owner>/<repo>` of the checkout's `origin` remote. A vault override changes where keys are read from, never the repository. |
| Enabling | Only the account's own machine config section `secrets.github_actions` enables the target. A tracked project file with a `github_actions:` section is refused. A sandboxed server opens no sync target. |
| Login | `gh` uses its own stored login. Inherited `GH_*`, `GITHUB_TOKEN` and `GITHUB_ENTERPRISE_TOKEN` variables are removed. A signed-out `gh` pushes nothing: every key is listed in `failed`, and its audit record says `backend_locked`. It is not a request-level error. |
| Value | The value reaches `gh secret set` on stdin only. It is never an argument. |
| Agents | A caller with a Claude Code ancestor is refused with `agent_use_refused`, whether or not the key is denied to agents. |
| Key names | `[A-Za-z0-9_]`, not starting with a digit or `GITHUB_`. GitHub stores them uppercased. |

The reply lists `copied` and `failed` keys. A key is `copied` only when the
secret's `updated_at` advanced after the push; exit status 0 alone does not
count. A failed key is listed in `failed` and the next key still runs.

The server writes one audit record per key, and one denial for a refusal
before any key moves. If the audit log cannot be opened, nothing is pushed.
If a record cannot be written after a push, the call stops with
`audit_unavailable` and returns no `copied` or `failed` list. That key and the
keys before it may already be in GitHub. After the request deadline, the
remaining keys fail without running.

## Safety rules

- No part of the crate prints or logs a secret value.
- Keep values out of agent transcripts. An agent session records what is typed
  into it and sends it to the model provider. Use a key name or reference.
- An agent may use every key in its scope unless the operator denied the key
  to agents (#10386). `set`, `delete`, removing a deny and a copy to a sync
  target are refused to a caller under Claude Code.

## Where it is going

The planned `tm secrets` commands and the trusty-console secrets page are
described in [Console Secrets Service (planned)](../trusty-console/secrets.md).
