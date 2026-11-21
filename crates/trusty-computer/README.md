# trusty-computer

A broker that lets local AI agents drive one operator-granted macOS app
through a bounded [Cua Driver](https://github.com/trycua/cua) 0.23.2 daemon.
An agent reads the app's Accessibility (AX) tree with the menu bar removed and
acts on elements by token. The broker checks, audits and journals every call.
It never returns a screenshot, pixels or the clipboard.

macOS only. Two binaries:

- `trusty-computer` runs the broker and the operator verbs.
- `trusty-computer-mcp` is a stdio MCP server with 20 `computer_*` tools. It
  talks only to the broker.

## What it does: Option B

The trusty-computer spike recommended "Option B": a broker in front of the
driver, with the driver's bounded mode as its daemon-side limit. In detail:

- The broker starts its own Cua Driver daemon through LaunchServices, so the
  daemon inherits `CuaDriver.app`'s macOS grants. The daemon runs in bounded
  mode, with a capability manifest that names the exact pid and window of the
  current grant, on a private socket in the state directory.
- The broker is the only client of that socket. Agents connect to
  `agent.sock`, the operator to `operator.sock`. Both are 0600 in a 0700 state
  directory, and a peer with another uid is refused.
- Each call passes the pipeline: `request_id` journal replay, the policy for
  all 56 driver tools (`invoke_menu`, screenshots, desktop state and the
  clipboard are denied), the grant's scope, the one desktop lease, the element
  token gate, an audit line, the driver call, menu-bar removal, and a
  read-back that confirms a mutating call.
- The audit log is fail-closed JSONL (0600, fsynced). It records argument
  keys and text lengths, never values.

The broker is a guardrail, not a security boundary. See "Known gaps".

## Prerequisite: Cua Driver and its macOS grants

Install Cua Driver **0.23.2** at `/Applications/CuaDriver.app`. The broker
refuses any other version, and `install-service` checks it.

The macOS grants belong to `CuaDriver.app` (bundle `com.trycua.driver`), not
to trusty-computer. To grant them, quit any running CuaDriver, then run
`cua-driver permissions grant` in Terminal at the Mac. It launches
CuaDriver.app through LaunchServices and shows, in order:

1. The Accessibility prompt ("Cua Driver would like to control this
   computer"). Click Open System Settings and turn Cua Driver on.
2. The Screen & System Audio Recording prompt. Turn it on; macOS may ask to
   quit and reopen the app.
3. On macOS 26, a direct-capture consent dialog, then a capture probe.

If Cua Driver is missing from the Screen Recording list, click **+** and add
`/Applications/CuaDriver.app`. The grants survive a daemon restart, a full
quit and a relaunch.

## Install

```bash
cargo install trusty-computer --version 0.1.0 --locked
trusty-computer install-service
```

`install-service [--state DIR] [--sign-identity ID | --no-sign]`:

1. Refuses a build with the `test-seams` feature, a plist it did not write,
   and a loaded `com.trusty.computer` job whose plist is elsewhere.
2. Signs `trusty-computer` and its sibling `trusty-computer-mcp` with the
   Developer ID of team `4JH68XUHC5`, identifier `com.trusty.computer`,
   `--options runtime`, and verifies both. The identity is `--sign-identity`,
   else `TRUSTY_COMPUTER_SIGN_IDENTITY`, else the one Developer ID Application
   identity of that team in the login keychain. `--no-sign` skips signing
   with a warning.
3. Checks that the installed Cua Driver is 0.23.2.
4. Writes `~/Library/LaunchAgents/com.trusty.computer.plist` (0600) and
   bootstraps it in `gui/<uid>`. The job runs
   `trusty-computer broker --state <dir>` with `KeepAlive`; the default state
   is `~/.trusty/computer`.

If signing, verification or the version check fails, nothing is written.
`uninstall-service [--state DIR]` boots the job out and removes its plist.

Never `cp` the binaries: copy a signed binary and macOS kills the next exec.

Point an MCP client at `trusty-computer-mcp [--state DIR]`.

## Consent and grants

An agent cannot grant itself an app. The operator mints a grant at a terminal:

```bash
trusty-computer grant <bundle-id> --pid N --window N [--ttl 30m] [--idle-ttl 5m] \
    [--tools a,b] [--allow-sensitive]
```

1. The CLI opens `/dev/tty` before it contacts the broker. With no terminal
   it exits 3 and sends nothing, so an agent running the CLI in Bash fails
   here.
2. The broker shows the app, tools, TTL and a six-digit code in an
   `osascript` dialog, run as a separate helper process. The operator clicks
   confirm and types the code at the terminal. A denied, timed-out or failed
   dialog, a wrong code or an expired code refuses.
3. The CLI prints a one-use `tc-` grant code (120 random bits, valid 300 s).
   The agent redeems it with `computer_connect`. A session reaches only the
   grants it redeemed.
4. Five bad codes in 60 s lock out minting and redeeming.

Browsers, terminals, Finder, System Settings and password managers need
`--allow-sensitive`. Grants persist in `grants.json` (0600) until they expire
or are revoked. Default TTL 30 min, idle TTL 5 min.

Operator verbs: `grants`, `revoke <grant-id>`, `status`, `lease`, and
`lockout`. `lockout` revokes every grant, drops every code and the lease,
stops the daemon and exits the broker.

## MCP tools

| Tool | What it does |
|---|---|
| `computer_connect` | Redeem a `tc-` code for a grant and ask for the desktop lease (held or queued). |
| `computer_status` | Lease state, queue position, grant TTL, driver version, telemetry state. |
| `computer_release` | Give up the lease to the next grant. |
| `computer_disconnect` | End the session and drop its grant bindings. |
| `computer_grants` | This session's grants (app, tools, expiry), never other grants. |
| `computer_apps` | Running pids of the granted app. |
| `computer_windows` | Windows of the granted app. |
| `computer_read` | AX tree of a granted window, menu bar removed, element tokens included. |
| `computer_verify` | Check a predicate on window state. |
| `computer_click` | Click an element by token in the background; `confirmed` by read-back. |
| `computer_double_click` | As click, double. |
| `computer_right_click` | As click, secondary button. |
| `computer_scroll` | Scroll an element by token. |
| `computer_type` | Insert up to 2000 characters into an element by token. |
| `computer_set_value` | Set an AX value on an element by token. |
| `computer_key` | One key to the granted app; quit, close, hide and switch chords refused. |
| `computer_hotkey` | A chord to the granted app, same refusals. |
| `computer_launch` | Launch the granted bundle id in the background. |
| `computer_wait` | Wait up to 5 s in the broker; keeps the lease. |
| `computer_refresh` | Re-list pids and rescope the daemon after a relaunch or a new window. |

Every action tool takes a `request_id`; the MCP server fills a uuid4 when it
is omitted. A repeated id is answered from the journal and not run again.

## Known gaps

- **An agent with Bash can still call its own `osascript` or
  `screencapture`.** Those run under the agent's own macOS grants, not the
  broker's. Only a sandbox profile for agent processes would narrow this.
- **A same-uid process can start its own unbounded cua-driver daemon.**
  `open -n -g -a CuaDriver` inherits CuaDriver.app's grants, and the driver
  checks only the peer uid. The operator accepted this gap.
- **AX returns no windows while the screen is locked.** Reads and actions
  fail until the operator unlocks the Mac.
- **Cua Driver telemetry is on by default.** The broker starts every daemon
  and every `cua-driver` client with `CUA_DRIVER_RS_TELEMETRY_ENABLED=false`,
  `CUA_TELEMETRY_ENABLED=false` and `CUA_DRIVER_RS_UPDATE_CHECK=false`. A
  daemon you start yourself does not get these; run
  `cua-driver telemetry disable` once to persist the setting in
  `~/.cua-driver/config.json`.

## Development

Live checks run only through `scripts/sandbox_computer_daemon.sh`, which gives
the broker a temporary HOME, so it never reaches the live `~/.cua-driver`.
Tests drive fakes of `launchctl`, `codesign` and the driver. The build plan is
`docs/plans/trusty-computer-phase2.md` (issue #11460).

## License

MIT
