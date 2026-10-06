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
backend). The first three are on by default. The
[crate README](../../crates/trusty-secrets/README.md) lists what each adds and
has a library example.

## Safety rules

- No part of the crate prints or logs a secret value.
- Keep values out of agent transcripts. An agent session records what is typed
  into it and sends it to the model provider. Use a key name or reference.
- A key's "agents may use" flag is off by default.

## Where it is going

The planned `tm secrets` commands and the trusty-console secrets page are
described in [Console Secrets Service (planned)](../trusty-console/secrets.md).
