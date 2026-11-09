# Architect presence

The pm-guard lets the bound Architect write a trust anchor only when Bob has
proven, recently, that he is at the Studio. This page covers the local path
(#8878 PR2a). Approval from other enrolled devices (passkeys through the
console) is #8878 PR2b and is not built yet.

Plans: `.claude/scratch-guard-twin/plan.md` §3–§5 and
`plan-pr2-devices.md` (owner answers D5b, 2026-11-09).

## What the guard requires

A trust-anchor write by the Architect's main thread (the `claude` with a
launch record, see `tm fleet status`) passes only when a valid presence grant
exists for that `claude`. A write that touches no anchor needs no grant.

When the grant is missing or invalid, the hook **denies** the call. The deny
text names `tm presence local`. The hook never returns `ask`: an `ask` blocks
an unattended pane and is answered by keystrokes any session can type.

A valid grant:

- is `~/.trusty-mpm/architect-launch/<pid>.presence`, a regular file of the
  user's, mode 0600, opened without following a symlink;
- names the same PID and process start time as the calling `claude` (a grant
  copied to another PID, or left behind by a reused PID, is refused);
- carries a valid HMAC-SHA256 seal made with the grant key;
- was used by a guarded call within the last 120 minutes (each guarded call
  refreshes it, at most once a minute).

Any other state denies: no grant, an unreadable or symlinked file, a
malformed file, no key, a bad seal, an unknown method, a future date, or 120
idle minutes.

When `CLAUDE_CODE_BRIDGE_SESSION_ID` is set, or the payload carries a
`bridge_status`, the deny adds a note that Remote Control appears active. The
note is information only; it grants nothing (owner ruling Q9b).

The absolute classes are unchanged and are never lifted by presence:
value-printing credential reads, channel taint, the question table, and
project-settings content.

## `tm presence local`

Run it from the Architect session at the Studio, for example by asking the
Architect to run it or with `! tm presence local` in its pane.

1. It checks that the session is the bound Architect (the same identity chain
   the hook uses, over the `claude` above the shell). Any other session gets
   `presence unavailable` with no prompt and no file.
2. It shows the macOS LocalAuthentication prompt (Touch ID, or the login
   password). Over SSH or with no GUI session it is `Unavailable` and writes
   nothing; a cancelled or timed-out prompt writes nothing.
3. On success it writes the grant, method `local`.

PMs, agents and subagents cannot run it: the pm-guard treats `tm presence`
and `tm fleet bind` as trust-anchor writes for every caller but the
Architect's main thread, and the anchor directory itself is sealed.

## `tm fleet bind`

Binds a running Architect `claude` that no tm launch recorded. Run it from the
Architect's own tmux pane. It refuses unless every identity check passes but
the launch record, and unless the `claude` under the pane is the one running
the command. After one Touch ID or password prompt it writes the launch record
(through `bind_architect`) and the presence grant. A bound session is left as
it is.

## The grant key

The key is 32 random bytes in the login Keychain, service
`trusty-mpm.presence`, account `grant-hmac-v1`. `tm presence local` creates it
on first use. macOS trusts only the binary that created the item, so another
program reading it shows an access prompt.

- A newly built `tm` is a new binary to the Keychain. Its first read shows the
  access prompt once; allow it for `tm` by running `tm presence local` at the
  Studio. A hook that meets the prompt does not wait for it: the guard's
  8-second deadline (release builds, GP-049 `guard-timeout`) or Claude Code's
  10-second hook timeout with `onFailure: "block"` (debug builds) ends the
  run, and the call is refused with the timeout text, not the
  `tm presence local` remedy.
- The key moves to the file `architect-launch/presence-hmac.key` in the
  scratch home only under the sandbox's full marker: `TRUSTY_SANDBOX=1`, a
  non-empty `TRUSTY_DATA_DIR_OVERRIDE`, and a `$HOME` that is not the
  account's own (`scripts/sandbox_daemon.sh` sets all three).
  `TRUSTY_SANDBOX=1` alone keeps the key in the Keychain.

### Who can replace the key

tm reads whatever item answers to service `trusty-mpm.presence`; it does not
check who created the item or its access list.

- Hooked callers — PMs, agents, subagents and the Architect — cannot. The
  pm-guard denies, on every path and under both bypasses, a `security
  add-generic-password`, `delete-generic-password` or
  `set-generic-password-partition-list` that names the item, chooses its
  words at run time, or names no literal service or account; and any
  Keychain API write (`keyring.set_password`, `SecItemAdd`, …) naming the
  item. The same check reads the body of every script the command runs.
- A process outside any hook, running as the same user, can. It can create
  the item before the first `tm presence local`, or delete it and create a
  replacement, and then seal its own grants. It can also replace the `tm`
  binary. Owner ruling Q9b accepts this: only Bob types commands outside a
  hook.
- Also not covered: a name an interpreter's code assembles at run time, a
  script the guard cannot locate, and a Keychain search list that puts
  another keychain first (`security list-keychains -s`).

## Not covered

- `tm run` launches are still not bound; a launch record without a tmux
  session changes the record format, and is deferred to #8878 PR2b-1.
- The idle limit is fixed at 120 minutes; there is no config key.
