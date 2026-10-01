# Console secrets service

**Status: in development; not yet released.** Nothing on this page is
available today. Every command, package name and screen described here is the
planned design, and names can change before release. Work is tracked in epic
[#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517).

## What it is

The secrets service stores API keys, tokens and passwords for your projects.
You enter each value in the trusty-console dashboard, in your browser. You
never type a value into an agent session.

An agent session records everything typed into it. A secret pasted into a
prompt or a shell command goes into the transcript and to the model provider.
The console keeps the value out of that path. Agents and your code refer to a
secret by its key name. The value goes into a process environment only when the
process starts.

The service is a library, not a new daemon. The `tm` daemon hosts it on its
existing local Unix socket. The console talks to it over that socket. No new
network port is opened.

## Concepts

### Scopes

Each secret belongs to one scope.

| Scope | Covers | Example |
|---|---|---|
| Project | One repository | `API_KEY` for `acme/web` only |
| Owner | Every project of one GitHub owner | `API_KEY` for every `acme/*` repository |

The owner is the GitHub user or organization in the project's git remote. A
project whose `origin` is `github.com/acme/web` has the owner `acme`. There is
no machine-wide scope.

When a project key and an owner key share a name, the project key wins. Set a
shared value once at the owner scope. Override it in a project that needs a
different value.

### Stores

A store is where the value lives. The service keeps no values itself.

| Store | Role |
|---|---|
| macOS Keychain | Default. Ships first. |
| 1Password | Planned integration |
| Keeper | Planned integration |
| Doppler | Planned integration |
| AWS Secrets Manager | Planned integration |

The service reaches the external stores through each vendor's own command-line
tool. It passes a value to that tool on standard input, never as a
command-line argument. Integrations arrive one at a time after the Keychain
release.

### Push targets

A push target receives a copy of a secret for deployment. The service writes to
a push target and never reads from it.

| Target | Use |
|---|---|
| Vercel | Project environment variables |
| GitHub Actions | Repository secrets |

Pushing to Vercel's development environment is opt-in for each key, because
Vercel keeps development values readable.

### Masking

The console never shows a stored value.

- The secret list shows each key's length and last-updated time.
- When you save a value, the console shows its first 8 characters once, so you
  can confirm you pasted the right thing.
- A value of 8 characters or fewer shows its length only, on save and in the
  list.

The service's own index stores no plaintext.

### The "agents may use" flag

Each key has an "agents may use" flag. It is off by default.

With the flag off, the service never puts the key into a process whose parent
chain includes Claude Code. Your own terminal can still use the key. An agent
session cannot. Tick the flag in the console only for keys an agent needs.

## Adding a secret

You add a secret on the Secrets page of the console. Pick the scope, enter the
key name, and paste the value.

From a terminal, the planned command reads the value from your clipboard:

```bash
# Planned interface — not available yet.
tm secrets set STRIPE_KEY            # value from the clipboard
tm secrets set STRIPE_KEY billing    # optional group label
printf '%s' "$VALUE" | tm secrets set STRIPE_KEY --value -   # value from stdin
```

`set` creates the key or updates it. An empty clipboard is an error, and
nothing is stored. The value never appears on the command line. The
confirmation line follows the masking rules above.

## Using secrets from code

Your code reads secrets as environment variables, from a process started by
`tm secrets exec`. Examples for Python, Node and `.env` files are in
[Using secrets from code](../guides/secrets-from-code.md).

## Console access

You reach the console in two ways:

- Locally, on the same machine as the `tm` daemon.
- Over Tailscale, from your own devices. Only your own tailnet login can use
  the Secrets page. Another user on the same tailnet cannot.

## Security model

Values are never returned to the console or to MCP tools. The console can
write a value and list key names. It cannot read a value back.

An agent with shell access and an exec grant for a key can still read that
key's value and print it. The flag limits which keys an agent can reach. It
cannot stop misuse of a key the agent was granted.

The full model, with its limits, is in
[Secrets security model](../guides/secrets-security-model.md).
