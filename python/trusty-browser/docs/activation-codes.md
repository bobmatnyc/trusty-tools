# Activation codes: broker frame contract

Status: phase 1 (broker core), issue #11239. The popup, the MCP shim and the
CLI build against this contract in later phases.

An activation code lets any local agent drive one enrolled Chrome profile, or
one tab in it, without holding the workflow ticket. The popup mints a code, a
person pastes it to an agent, and the agent redeems it on `agent.sock`. A code
is single use. The broker keeps codes and grants in memory only, so a broker
restart revokes every code and grant. No new socket, process, launchd job,
file holding a secret or Keychain item exists.

All frames use the existing length-prefixed JSON framing. Every socket still
requires a same-uid Unix peer (`verify_peer`). Requests with unknown or missing
fields are refused.

## Terms

- **Code**: `tb-` followed by four groups of six Crockford base32 characters,
  for example `tb-7K3M9Q-1ZXD4R-H8V2NB-QW6TPA`. 120 random bits from `secrets`.
  Redeem accepts lower case, ignores `-`, and reads `I`/`L` as `1` and `O` as
  `0`. The broker stores only its SHA-256, in memory.
- **Grant id**: a non-secret id such as `g-3f9a1c2b7d0e`. The unredeemed code
  and the grant it becomes share one id. Operators revoke by this id.
- **Scope**: `profile` (every tab of the minting profile's enrolled route) or
  `tab` (one tab incarnation). A tab incarnation is the Chrome tab id inside
  one browser pairing generation (`peer.generation`). The broker does not see
  Chrome's `documentId`; a navigation inside the same tab keeps the
  incarnation, a closed or replaced tab, or a browser restart, ends it.
- Constants (`grants.py`): `CODE_TTL = 300` s, `GRANT_IDLE = 1800` s,
  `MAX_GRANTS_PER_TAB = 1`, `MAX_GRANTS = 3`, lockout `FAIL_LIMIT = 5` failures
  in `FAIL_WINDOW = 60` s, first lock `LOCK_BASE = 60` s, doubling per repeat
  lock up to `LOCK_MAX = 900` s.

## mint — `enrollment.sock` only

Sender: the extension popup, through the verified Chrome native host. The
broker runs the same peer and Chrome-host verification as enrollment before it
reads the frame. `agent.sock`, `service.sock` and `operator.sock` refuse
`mint`.

Request: the enrolled registration, which proves the sender holds the profile's
browser key, plus the scope.

```json
{"op": "mint", "profile": "<uuid>", "email": "...", "workflow": "...",
 "url": "...", "account": "...", "key": "<browser key hex>",
 "scope": "profile"}
{"op": "mint", "...": "same fields", "scope": "tab", "tab": 7}
```

`tab` is present exactly when `scope` is `tab`, and is a non-negative integer.

Reply:

```json
{"ok": true, "code": "tb-...", "grant": "g-...", "scope": "tab",
 "profile": "<uuid>", "tab": 7, "expires_at": 1763140000.0}
```

`expires_at` is Unix seconds. `tab` appears only for tab scope.

Errors (`{"ok": false, "error": <name>}`):

| Error | Cause |
|---|---|
| `Unauthorized` | Peer or Chrome-host verification failed (existing) |
| `InvalidMint` | Malformed frame, unknown scope, bad or misplaced `tab` |
| `NotEnrolled` | No active route matches the presented registration |
| `BrowserDisconnected` | Tab scope and no paired browser for the profile |
| `GrantLimit` | A live grant already holds this tab, or `MAX_GRANTS` are live; carries `"limit": "tab"` or `"total"` |
| `AuditUnavailable` | The audit line could not be written; no code exists |

## redeem — `agent.sock`, first frame only

Sender: any same-uid agent process. `redeem` replaces `open` as the first
frame; `{"op": "open", "workflow", "ticket"}` is unchanged.

Request: `{"op": "redeem", "code": "tb-..."}`

Reply:

```json
{"ok": true, "grant": "g-...", "scope": "profile", "profile": "<uuid>",
 "idle_timeout_s": 1800}
```

Tab scope adds `"tab": 7`. The grant belongs to this socket connection only.
Redeem consumes the code atomically: of two concurrent redeems, exactly one
wins. On any refusal the broker writes the error frame and closes the
connection.

| Error | Cause |
|---|---|
| `InvalidRequest` | Malformed frame |
| `CodeRefused` | Wrong, already used, expired, or being redeemed by another connection. The agent cannot tell which; the audit log can |
| `RedeemLocked` | Lockout is active; carries `retry_after_s`. A correct code is refused too |
| `TabClosed` | Tab scope: the tab incarnation ended before redeem. The code is consumed and never bound to another tab |
| `BrowserDisconnected` | Profile scope: no paired browser; the code stays redeemable until it expires |
| `GrantLimit` | Redeem would exceed a cap; the code stays redeemable |
| `AuditUnavailable` | The audit line could not be written; the code stays redeemable |

Lockout: `CodeRefused` outcomes count as failures. `FAIL_LIMIT` failures
within `FAIL_WINDOW` lock redeem for every connection. Attempts during a lock
are refused without counting, so a lock always expires. Each lock that follows
within `LOCK_MAX` of the previous one doubles, up to `LOCK_MAX`; a successful
redeem, or `LOCK_MAX` without a lock, resets the length. Mint is never locked.

## Operations on a redeemed session

The session uses the existing `claim`, `snapshot`, `click` and `release`
frames, the existing lease tokens, and the existing URL floor (`https`, or
`http` on `127.0.0.1`). Competing-claim refusal (`TabClaimed`) applies between
code sessions and workflow sessions alike.

New errors on these operations:

| Error | Cause |
|---|---|
| `GrantEnded` | The grant is gone; carries `reason`. The broker closes the connection after this frame |
| `OutOfScope` | Tab scope: the profile's enrolled tab is now a different tab; nothing is sent to that tab |
| `AuditUnavailable` | The operation's audit line could not be written; the operation did not run |

`GrantEnded.reason` is one of `revoked`, `tab_closed` (the granted tab
answered `TabNotFound`), `browser_gone` (the profile's pairing ended or was
replaced), `unenrolled` (the route was removed or replaced), `idle` (no
operation for `GRANT_IDLE`), `audit` (an operation's result line could not be
written). A grant also ends silently when its socket closes. When a grant ends
its claims are dropped and their indicators cleared. A click in flight when its
grant ends answers `OutcomeUnknown`, as a click does today when its browser
disconnects; a snapshot in flight answers `GrantEnded`.

## Operator ops — `operator.sock`

Sender: the local operator CLI (later phase). The existing
`{"op": "revoke", "workflow"}` is unchanged.

| Request | Reply |
|---|---|
| `{"op": "grants"}` | `{"ok": true, "grants": [entry, ...]}` |
| `{"op": "revoke_grant", "grant": "g-..."}` | `{"ok": true}` or `{"ok": false, "error": "UnknownGrant"}` |
| `{"op": "revoke_grants"}` | `{"ok": true, "revoked": <count>}` |

An entry is `{"grant", "scope", "profile", "tab" (tab scope only), "state":
"code" | "active", "expires_in_s" (code) or "idle_s" (active)}`. It never
carries a code or a hash. Revoke applies to an unredeemed code or a live grant.
Malformed frames answer `RevokeFailed`, as today.

## Audit log

`<state>/audit.jsonl`, mode 0600, opened `O_APPEND | O_NOFOLLOW`, rotated at
5 MiB to `audit.jsonl.1` and `audit.jsonl.2` (two kept). One JSON object per
line with only these keys: `ts`, `event`, `grant`, `scope`, `profile`, `op`,
`tab`, `outcome`, `origin` (URL host only). It never carries a code, a code
hash, a ticket, a lease token, page content, or a URL path or query.

Events: `mint`, `redeem` (outcome `ok`, `wrong`, `replay`, `expired`,
`contended`, `locked`, `tab_closed`, `browser_disconnected`, `limit`), `lock`,
`revoke`, `grant_end` (outcome is the reason, or `closed`), `operation`
(outcome `admitted`, then a second line with `ok` or the error name).

Failure policy: mint, redeem and each operation's `admitted` line are
fail-closed — if the write fails the request is refused with
`AuditUnavailable` and nothing is created, consumed or run. A failed result
line ends the grant (`reason: audit`). Revoke, lock and grant-end lines are
best-effort: a failed write never blocks a revoke or a lock.
