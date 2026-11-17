# Secrets security model

**Status: in development; not yet released.** This page describes the planned
design. Background:
[Console secrets service](../trusty-console/secrets.md).

## What the service protects

The service keeps secret values out of agent transcripts. You enter a value in
the console, not in an agent session. Agents and code refer to the key name.
The value goes into a process environment only when `tm secrets exec` starts
that process.

| Path | Can it read a value? |
|---|---|
| The console in your browser | No. It writes values and lists key names, lengths and update times. |
| MCP tools an agent calls | No. They never return a value. |
| A process started by `tm secrets exec` | Yes, for the keys it is allowed. |
| A language client inside that process | Yes, for the keys it is allowed. |
| A language client in any other process | No. |

The service's own index stores no plaintext. Values live in the store you
choose: the macOS Keychain by default.

## Agents and the per-key deny

The model is the same as `.env.local`: a key in your scope (the project or
owner vault) is usable by an agent, unless you deny that key to agents. A
denied key is never set in a process whose parent chain includes Claude Code.
You tick "Denied to agents" in the console. An agent may deny a key; only a
person can remove a deny (#10386).

Some operations stay with a person whatever the key: deleting a key,
removing a deny, and copying keys to a sync target such as GitHub Actions.
The service refuses each of them to a caller under Claude Code. An agent may
store a value (`set`, `import`) or copy keys to another local backend only
for keys you have not denied (#10459); its confirmation shows the length
only, and the audit log records its pid and project. A copy keeps a key's deny; a denied key cannot be copied into 1Password
or Keeper, which cannot hold a deny.

## What it does not protect

An agent with shell access and an exec grant can read any value it was
granted. For example, it can run `tm secrets exec -- printenv STRIPE_KEY` and
copy the output into its transcript. The deny decides which keys an agent can
reach. It does not stop the agent from printing one of those keys.

Treat every key without a deny as a key the agent can read. Deny the keys an
agent does not need. Prefer keys with narrow permissions for agent work.

A process started by `tm secrets exec` holds plain values in its environment.
Any code in that process, and any child it starts, can read them.

## Console access

The console serves the Secrets page in two places:

- On the local machine.
- Over Tailscale, for your own tailnet login only.

The console runs no secrets logic itself. It passes requests to the `tm`
daemon over a local Unix socket, which accepts connections from your own user
only. Before secrets go through the console, its web routes gain these checks:

- requests from other web origins are refused;
- requests with an unexpected `Host` header are refused;
- each console launch issues a new anti-forgery token;
- error messages use fixed text and never echo request data.

## Stores and push targets

External stores receive values on their command-line tool's standard input,
never as an argument, so a value does not appear in a process listing. Push
targets such as Vercel and GitHub Actions only receive values. The service
never reads a value back from them.
