# Architect presence

The pm-guard lets the bound Architect write a trust anchor only when Bob has
proven, recently, that he is at the Studio. This page covers the local path
(#8878 PR2a), approval from enrolled passkey devices (#8878 PR2b-1) and
enrolment through a one-time browser link (#8878 PR2b-2). The browser page
that lets a phone approve a request, and Slack delivery of its link, are
#8878 PR2b-3 and are not built yet.

**key ACL unverified (#10865): device approvals are untrusted.** Every device
surface prints this until #10865 verifies the grant key's Keychain access
list (see "Who can replace the key").

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

- Hooked callers — PMs, agents, subagents and the Architect — cannot, except
  through the residuals below. The pm-guard denies, on every path and under
  both bypasses, every `security` call whose verb is not a complete literal
  from a read-only allowlist: `find-generic-password`,
  `find-internet-password`, `find-identity`, `find-certificate` and `help`.
  It reads every `security` word, not only one in command position, so
  `echo 'security …' | sh` is read too. Redirections with their targets and
  `security`'s own options are skipped first; `-i` and `-p` (interactive
  mode) deny. An unknown, abbreviated, globbed or run-time-built verb denies.
  Two shapes pass because `security` runs nothing for them: no verb (it
  prints usage) and a plain word that is no verb nor a verb's prefix, such as
  the prose in `git commit -m "harden security for the guard"`. Printing a
  secret with `find-generic-password -w` stays the credential-print rule's
  deny. The guard also denies any Keychain API write
  (`keyring.set_password`, `SecItemAdd`, …) naming the item, and reads the
  body of every script the command runs.
- A process outside any hook, running as the same user, can. It can create
  the item before the first `tm presence local`, or delete it and create a
  replacement, and then seal its own grants. It can also replace the `tm`
  binary. Owner ruling Q9b accepts this: only Bob types commands outside a
  hook.
- Residuals for hooked callers, accepted for PR2a:
  - an interpreter that assembles the service name, or the command name, at
    run time (`python3 -c` with `keyring`, `osascript`), or a script the
    guard cannot locate;
  - a command name the shell builds (`S=security; $S …`,
    `/usr/bin/s?curity`);
  - through either, a Keychain search list that puts another keychain first
    (`security list-keychains -s`), which shadows the item.

  Follow-up #10865 makes tm check the item's access list
  (`SecKeychainItemCopyAccess`), which closes them. Until it ships, the key is
  not trusted for any approval decision. A forged grant only restores
  pre-PR2a behaviour: the Architect's anchor writes needed no grant on main,
  and no call that is denied without a grant is approved because of one.

## Device approvals (#8878 PR2b-1)

An enrolled passkey (Face ID or Touch ID on an iPhone, iPad or Mac) can
approve one pending request for one `claude`, once #10865 lands: until then
the daemon refuses every approval (owner ruling D24, see "Enrolment"). The approval becomes a grant
with method `device:<credential id>`, which the hook accepts like `local`
(owner ruling D67) while the device's sealed record exists. It lifts exactly
what `local` lifts and never an absolute class.

### Flow

1. A guarded call by the Architect has no valid grant. The hook denies it and
   asks the daemon, within one second, for a pending request (only from the
   account's real home). If the request lands, the deny names its id and
   `tm presence wait --request <id>`. If it fails (daemon down, timeout,
   rate limit), the deny text is unchanged and nothing is granted.
   `tm presence request` asks the same way from the command line.
2. The daemon writes `architect-launch/pending/<id>.json` (0600, sealed),
   but only for a live PID with the stated start time. A request expires
   after `[presence] request_ttl_minutes` (default 5). At most 16 are live,
   one per PID per 2 minutes.
3. An approver loads the request (`mpm.presence.approve`). The daemon opens a
   challenge for that request, held in memory only, and the passkey options.
   The action text comes from the caller and is always shown labelled
   "unverified origin".
4. The device signs. `mpm.presence.verify` checks, in a fixed order: the key,
   the open challenge, the request's seal and dates, the action hash, the
   live `claude`, the sealed device record, the passkey signature (origin, RP
   id, user verification), a counter that moves forward, then saves the
   counter, consumes the request by an atomic rename and writes the grant.
5. `tm presence wait` sees the grant through the hook's own check.

Every refusal writes no grant and leaves the request in place. A daemon
restart drops every open challenge, so a verify after it is refused.

### Passkeys: ES256 only

The verifier is webauthn-rs 0.6.2. Registration offers ES256 (ECDSA P-256)
only and refuses any other credential before a record is written; verify
refuses a record whose key is not ES256 before any signature check (owner
ruling D72). webauthn-rs pulls the `rsa` crate (RUSTSEC-2023-0071, ignored
in `deny.toml` and `.cargo/audit.toml` until 2027-02-07): no RSA decryption
runs; no RSA credential is stored or used at approval.

`[presence] rp_id` (the console's tailnet name) and `rp_origin`
(`https://<rp_id>`) have no default. With either missing, the daemon's
device methods answer exit 3 and nothing is enrolled or approved.

### Commands

| Command | Does | Exit |
|---|---|---|
| `tm presence request [--action <text>]` | Asks for a device approval for this session's `claude`; grants nothing | 0 asked, 1 refused |
| `tm presence wait [--timeout 5m] [--request <id>]` | Polls for a valid grant | 0 granted, 75 still pending (re-issue), 1 the request is gone with no grant |
| `tm presence devices add <name>` | Touch ID, then a one-time enrol link (see "Enrolment"). With `browser_enrol = false` or no `rp_origin`: refuses before the prompt | 0, 1 |
| `tm presence devices list` | Each device: id, name, created, last used, counter, backup flags | 0 |
| `tm presence devices revoke <id>` | Touch ID, then deletes the record and every grant it approved | 0, 1 |

PMs, agents and subagents cannot run any `tm presence` verb, and cannot write
`devices/`, `pending/` or a `.presence` file: all are trust-anchor writes.

### Enrolment (#8878 PR2b-2)

Browser enrolment is on by default (owner ruling D24) once `rp_id` and
`rp_origin` are set. One key in `~/.trusty-mpm/config.toml` turns it off;
restart the daemon after changing it (it reads `[presence]` at start):

```toml
[presence]
browser_enrol = false
```

With it off, or with `rp_origin` unset, `tm presence devices add` says
"enrolment via browser is not enabled" before any prompt, and the console
answers `404` for the page and its calls.

An enrolled device approves nothing yet: until #10865 verifies the key's
Keychain access list, the daemon's `mpm.presence.approve` and
`mpm.presence.verify` refuse every request and no device grant is written
(owner ruling D24).

With it on:

1. `tm presence devices add <name>` runs Touch ID, mints an enrol ticket,
   checks that the daemon opens a registration for it, and prints
   `<rp_origin>/tools/presence/enrol/#<ticket>`. The link is valid once, for
   10 minutes (the ticket's own TTL).
2. Open the link on the device. The ticket rides in the URL fragment, which
   the browser never sends to a server; the page removes it from the address
   bar and history at once.
3. The page posts the ticket to the console, which asks the daemon for
   `mpm.presence.register.start` and shows the passkey prompt. The passkey
   is bound to the page's origin, so it must be `rp_origin` (the console's
   https tailnet name); on `127.0.0.1` the browser cannot create it.
4. The page posts the credential; the console relays it to
   `mpm.presence.register.finish`. The daemon checks the open registration,
   the passkey (origin, RP id, user verification, ES256), then the ticket,
   marks it spent and writes the sealed record. The page says "Enrolled"
   only when the daemon confirms the credential id.

The console asks the daemon's `mpm.presence.status` for the enrol origin on
every request, and serves the page or relays a call only when `Host` names
that origin and a POST carries it as `Origin` (`403` otherwise). This holds
on every listener; it does not rest on the tailnet peer gate. A request
without a ticket never reaches the daemon.

Refusals, each writing no device record:

- a used link: `register.start` refuses a spent ticket, and a second finish
  finds no open registration;
- an expired link, or a ticket dated more than a minute ahead or valid longer
  than 10 minutes;
- a finish after a daemon restart (registrations are held in memory only);
- a finish that fails any check: it consumes the open registration, the
  ticket stays unspent, and reloading the link starts again;
- a ticket for another device name;
- a credential that is not ES256.

The ticket is spent before the record is written, so a failed write leaves
a spent ticket and no device; run `devices add` again.

### Tickets

`devices add` and `devices revoke` need a sealed local-presence ticket, minted
by the CLI only after the LocalAuthentication prompt answered. It is valid 10
minutes, for one purpose and one device. An enrol ticket enrols one device
only (`devices/.ticket-<nonce>` marks it spent). A caller that reaches the
daemon socket directly, skipping the CLI, has no ticket.

### Revoke

Revoke deletes the device record first, then every `.presence` whose method
names it, under the device store lock, so an approval being verified cannot
mint after the sweep. A grant whose removal failed is still refused: the hook
re-reads the device record. Revoke by another enrolled device is PR2b-3.

### Recovery

A lost phone: run `tm presence devices revoke <id>` at the Studio. No device
works: `tm presence local` at the Studio always works. A replaced or missing
grant key makes every sealed record, request and grant unreadable; enrol
again.

## Not covered

- Approving a request from the phone's browser, Slack delivery of links,
  device-authorised revoke and the `approve` ceremony-replacement fix are
  #8878 PR2b-3.
- A real-device enrolment (Touch ID in Safari on the Studio, Face ID on an
  iPhone) is a manual owner check; the automated smoke uses a software
  passkey (`crates/trusty-mpm/tests/presence_enrol_smoke_8878.rs`).

- `tm run` launches are still not bound; a launch record without a tmux
  session changes the record format, and is deferred to #8878 PR2b-1.
- The idle limit is fixed at 120 minutes; there is no config key.
