# Using secrets from code

**Status: in development; not yet released.** Every command and package on
this page is the planned interface. None of it is available today, and the
package names are proposed. Background:
[Console secrets service](../trusty-console/secrets.md).

Your code never calls the console. It reads secrets from a process that
`tm secrets exec` starts. There are three ways to do that.

| Way | How the value arrives | Use it for |
|---|---|---|
| Environment variables | `tm secrets exec -- <cmd>` sets one variable per key | Most programs |
| `.env` references | `tm secrets exec --dotenv .env -- <cmd>` resolves `secret://` entries | Projects that already use a `.env` file |
| Language clients | A client library asks for a key at run time | Code that needs a key only on some paths |

Each key resolves in the project's scope first, then the owner's scope.

## Environment variables

`tm secrets exec` looks up the project's keys, sets each one as an environment
variable, and starts your command.

```bash
# Planned interface — not available yet.
tm secrets exec -- python app.py
tm secrets exec -- node server.js
tm secrets exec -- cargo run
```

Your code reads the variable the normal way.

Python:

```python
# Planned interface: run with `tm secrets exec -- python app.py`.
import os

api_key = os.environ["STRIPE_KEY"]
```

Node:

```js
// Planned interface: run with `tm secrets exec -- node server.js`.
const apiKey = process.env.STRIPE_KEY;
if (!apiKey) throw new Error('STRIPE_KEY is not set');
```

The values exist only in the environment of the started process and its
children.

## `.env` files with `secret://` references

Keep your `.env` file. Replace each secret value with a `secret://` reference
to a key. A reference is a key name, not a value, so the file is safe to
commit.

```bash
# .env — planned interface
STRIPE_KEY=secret://STRIPE_KEY
DATABASE_URL=secret://DATABASE_URL
LOG_LEVEL=info
```

Start your program with `--dotenv`:

```bash
# Planned interface — not available yet.
tm secrets exec --dotenv .env -- npm run dev
```

`tm secrets exec` reads the file, resolves each `secret://` entry, and passes
the plain entries through unchanged. Your program sees `STRIPE_KEY`,
`DATABASE_URL` and `LOG_LEVEL` as ordinary environment variables. Your program
does not need a dotenv library for this.

## Language clients

A client library fetches one key while your program runs, instead of at start.
Three are planned. All names are proposed.

| Language | Proposed package |
|---|---|
| Rust | the `client` feature of the `trusty-secrets` crate |
| Python | `trusty-secrets` on PyPI, with no dependencies |
| JavaScript / TypeScript | `@trusty-tools/secrets` on npm, with no dependencies |

A client works only inside a process that `tm secrets exec` started. The
service checks a token, the key, and the process ancestry. A client
call from any other process fails. The client API is not designed yet, so this
page shows no client code.

## Agents and your code

A key whose "agents may use" flag is off is never set in a process whose
parent chain includes Claude Code. If an agent runs your tests and a key is
missing, check that key's flag in the console. Tick it only if the agent needs
the key. See [Secrets security model](secrets-security-model.md) for what the
flag does and does not protect.
