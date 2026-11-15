# Activation codes: broker frame contract

Status: phase 1 (broker core) is built. Phase 2, this document's page
operations, is the build contract for issue #11239: the popup mint flow, the
MCP connect flow, and tabs list, tab select, read, click, type and navigate on
ordinary pages. The broker side of phase 2 (pairing, page operations, the MCP
page tools and the operator CLI verbs) is built; the extension side is built
against the same text. A Python builder and a JavaScript builder build
against it in parallel; see "File ownership". Names marked provisional may
change before release, but both sides use them exactly as written here.

An activation code lets any local agent drive one paired Chrome profile, or
one tab in it, without holding the workflow ticket. The popup mints a code, a
person pastes it to an agent, and the agent redeems it on `agent.sock`. A code
is single use. The broker keeps codes and grants in memory only, so a broker
restart revokes every code and grant. No new socket, process, launchd job,
file holding a code, or Keychain item exists.

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
- **Profile credential**: the triple `profile` (UUID v4), `email` (the
  profile's signed-in Chrome account, lower case) and `key` (64 lower-case hex
  characters) that the extension holds in `chrome.storage.local`. It comes
  from the profile's workflow `enrollment` when one exists, else from its
  `pairing` (see "pair"). A profile holds one credential, never both kinds.
- **Scope**: `profile` (every tab of the paired profile) or `tab` (one tab
  incarnation).
- **Tab incarnation**: the Chrome tab id inside one browser pairing generation
  (`peer.generation`). A navigation inside the same tab keeps the incarnation;
  a closed or replaced tab, or a browser restart, ends it. See "Tab
  incarnation and documentId".
- **Page access**: the right to inject into a tab's top document. It comes
  from either of two Chrome grants. `activeTab`: the user clicks the extension
  action on that tab; it covers the origin shown at the click, survives popup
  close, same-origin navigation, reload and tab switch, ends on any
  cross-origin navigation (a different port counts) and does not return when
  the tab goes back. The optional `https://*/*` host permission: requested in
  the click that mints a profile code and held while the profile has a live code or
  grant (see "All-https access"); it covers every `https:` page of the
  profile.
- Constants (`codes.py`): `CODE_TTL = 300` s, `GRANT_IDLE = 1800` s,
  `MAX_GRANTS_PER_TAB = 1`, lockout `FAIL_LIMIT = 5` failures
  in `FAIL_WINDOW = 60` s, first lock `LOCK_BASE = 60` s, doubling per repeat
  lock up to `LOCK_MAX = 900` s. Lease `TTL = 60` s (`leases.py`). There is no
  cap on the total number of grants.

## Build constraints

- Manifest permissions stay exactly: `storage`, `nativeMessaging`, `identity`,
  `identity.email`, `alarms`, `scripting`, `activeTab`. No `host_permissions`.
  The optional `https://*/*` stays declared; the code flow requests and removes
  it as "All-https access" states. No manifest change.
- Page code runs only through `chrome.scripting.executeScript` in the default
  `ISOLATED` world, on the top frame, of a tab the worker names by explicit id.
  No operation looks up "the active tab" at operation time. The popup reads
  the active tab once, at popup load, and passes its id explicitly.
- Events dispatched into a page are synthetic (`isTrusted: false`).
- `chrome.tabs.update(tabId, {url})` navigates any tab; it needs no permission
  and is confined only by the checks in this contract.
- Without the `tabs` permission, `chrome.tabs.query` returns no `url` or
  `title` for a tab without page access. The worker learns a tab's origin and
  title only through a successful injected probe.

## All-https access

Owner ruling, 2026-11-14: "Ask for all-https access at Activate, drop it when
the last code ends." Any profile where the user clicks **Pair this profile**
may be paired; tab scope is the default choice. This contract reads "Activate" in that
ruling as the click that issues a profile code: the permission is requested
only in a profile mint click, never in a tab mint click and never at pairing, so the pairing acknowledgement that follows
`pair` (which lists no code) has nothing to remove.

Request:

- Owner ruling Dki, 2026-11-14: "tab scope by default". **Give access to this
  tab** requests no host permission; its grant relies on `activeTab` from the
  action click. Chrome shows `https://*/*` as "Read and change all your data
  on all websites", and that prompt closes the popup, so no tab code would be
  issued.
- The popup calls `chrome.permissions.request({origins: ["https://*/*"]})`
  inside the click handler of **Give access to the whole profile**, before it
  sends `mint`, whenever the permission is absent. It needs the user gesture, which the service worker
  does not have. This holds for a paired profile and for a workflow-enrolled
  one alike. The **Pair this profile** click sends `pair` only and requests
  nothing.
- Declining does not block minting; page operations outside the clicked
  origin then answer `PermissionNeeded`.

Removal. The worker calls `chrome.permissions.remove({origins:
["https://*/*"]})` when it learns that the profile holds no unredeemed code
and no live grant, and no mint is in flight. It learns that from exactly
these signals:

1. `grant_end` with `"last": true` (see "Browser hop"). The broker sends
   `grant_end` on every end route: agent release of the session
   (disconnect, socket close: reason `closed`), operator or popup revoke
   (`revoked`), idle expiry (`idle`), tab closed (`tab_closed`), worker
   `UnknownGrant` (`browser_gone`), result-line audit failure (`audit`),
   unpair or unenroll (`unenrolled`), and unredeemed code expiry
   (`expired`). `last` is true when, after this end, the profile holds no
   unredeemed code and no live grant.
2. The `service.sock` pairing acknowledgement, which carries
   `"live": [grant id, ...]`: every unredeemed code and live grant of the
   profile. The worker prunes its grant table to this list on every
   (re)connect, and removes the permission when the list is empty. This
   covers every `grant_end` the worker missed while its port was down,
   including a broker restart (which ends every code and grant).
3. A popup `grants` reply (see "Grant list and revoke from the popup") whose
   list is empty. The worker sends `grants` itself, outside the popup's
   5-second refresh, in two cases the first two signals cannot settle: a
   mint that failed after the permission was granted in its click (the
   broker's refusal says nothing about other codes), and a `grant_end` with
   `last: true` that leaves a row in the worker's table (a mint racing the
   end, or an end the worker missed). The broker list is the answer in both.
4. No credential. A profile whose `pairing` and `enrollment` are both absent
   can hold no code, so the worker removes the permission when `unpair`
   finishes and whenever a popup action (`pair`, `unpair`) fails and leaves
   no credential.
5. The offline check. While the native port is down the broker can send
   nothing, yet then holds nothing of the profile once every code has
   expired: the port's loss ends every grant of its pairing generation
   (`browser_gone`), redeem needs a connected browser, and a code expires
   `CODE_TTL` after its mint. The worker records when the port dropped, moves
   that time forward on each successful mint, and empties its table and
   removes the permission when it has gone `OFFLINE_QUIET` without an
   acknowledgement. `OFFLINE_QUIET = CODE_TTL + 60` s, which is 360 s; the
   60 s margin covers a redeem probe in flight (`PAGE_WAIT`, 8 s) and the
   worker's one-minute reconnect alarm, on which the check runs. The worker
   writes the figure as `QUIET_MS = 360000` in `extension/codes.js`;
   `tests/test_activation_codes.py`
   `test_the_worker_offline_quiet_matches_the_code_lifetime` fails when either
   side changes without the other.
6. A failed worker `grants` call (signal 3) while the native port is up:
   a timeout, a host refusal, an invalid reply, or a changed profile. The
   worker keeps its table and removes the permission when that table is
   empty and no mint is in flight. A grant absent from the table answers
   `UnknownGrant` to every page operation, so no live code or grant can use
   the permission. A row left in the table keeps it; that row ends through
   its own `grant_end` (port up) or the offline check (port down).

The popup reads the permission state at each profile mint click. It re-reads
it after every revoke and every grant poll, and follows
`chrome.permissions.onAdded` and `onRemoved`, so a removal by any of these
signals makes the next profile mint click ask again.

The broker cannot replace 3, 4, 5 or 6 cheaply: 3 would need a `live` list on
every mint refusal, 4 is local state the broker never sees, 5 is the
case where the broker is unreachable, and 6 is the case where the broker's
answer did not arrive.

- Idle expiry and code expiry reach the worker without waiting for an agent
  call: the broker keeps one deadline callback on its own event loop (no
  thread, no process) set to the earliest code deadline or grant idle
  deadline, and ends what is due when it fires. It is re-armed after every
  mint, every redeem (granted or refused) and every sweep, and cancelled with
  its in-flight sweep when the broker closes. A sweep that raises is logged
  and still re-arms.
- `unpair` removes the permission too; the worker does it on the `unpair`
  reply.
- Scope is unchanged by the permission: each agent stays inside its own
  code's scope, enforced independently by the broker and by the worker.

## pair — `enrollment.sock` only

Sender: the extension worker, for a profile with no workflow enrollment, when
the user clicks **Pair this profile** in the popup. Same peer and Chrome-host verification
as enrollment.

| Request | Reply |
|---|---|
| `{"op": "pair", "profile": "<uuid>", "email": "<str>", "key": "<64 hex>"}` | `{"ok": true, "profile": "<uuid>"}` |
| `{"op": "unpair", "profile": "<uuid>", "email": "<str>", "key": "<64 hex>"}` | `{"ok": true, "profile": "<uuid>"}` |

The worker generates `profile` (`crypto.randomUUID()`) and `key` (32 random
bytes, hex), writes them to `chrome.storage.local` as `pairing_pending`, sends
`pair`, retries with the same fields after an interruption, and on `ok` moves
them to `pairing`. The broker stores the key sealed with the existing vault,
as it does for an enrollment browser key. `pair` with fields identical to a
stored pairing answers `ok` and changes nothing. After `ok` the worker
reconnects its native port; the `service.sock` hello accepts a pairing
credential exactly as it accepts an enrollment one.

`unpair` removes the pairing and ends every code and grant of the profile with
`GrantEnded.reason: unenrolled`, closes their agent sessions and closes the
profile's `service.sock` connection. The worker then deletes `pairing` and
reconnects. A workflow enrollment is removed only by the existing `unenroll`.
An `unpair` retried after a lost `ok` reply answers `Unauthorized`, because
the pairing is already gone; the worker treats that reply to a retry as done.

The registry stores, per paired profile, exactly one row: `profile`, `email`
and the browser key sealed with the vault (purpose `browser:<profile>`, the
same purpose as an enrollment browser key). It never stores a code, a code
hash or a grant. `enroll` and `replace` for a profile or email that holds a
pairing answer `EnrollmentConflict`.

| Error | Cause |
|---|---|
| `Unauthorized` | Peer or Chrome-host verification failed, or `unpair` presented a credential that matches no pairing (existing name) |
| `InvalidEnrollment` | Malformed frame: unknown or missing field, `profile` not a UUID v4, `email` without `@`, `key` not 64 lower-case hex (existing name) |
| `EnrollmentConflict` | `pair`: the profile or email already holds a pairing with other fields, or a workflow enrollment (existing name) |

The worker never sends a workflow `enroll`, `activate` or `replace` while
`pairing` exists; the popup hides the workflow controls in that state.

## mint — `enrollment.sock` only

Sender: the extension popup, through the worker and the verified Chrome native
host. The broker runs the same peer and Chrome-host verification as enrollment
before it reads the frame. `agent.sock`, `service.sock` and `operator.sock`
refuse `mint`.

Request: the profile credential, which proves the sender holds the profile's
browser key, plus the scope. The credential triple is the form the worker
sends, so a paired profile and an enrolled one mint alike. The broker also
still accepts phase 1's six-field registration (`profile`, `email`,
`workflow`, `url`, `account`, `key`) for an enrolled profile; a paired
profile has no six-field form.

```json
{"op": "mint", "profile": "<uuid>", "email": "...", "key": "<64 hex>",
 "scope": "profile"}
{"op": "mint", "profile": "<uuid>", "email": "...", "key": "<64 hex>",
 "scope": "tab", "tab": 7}
```

`tab` is present exactly when `scope` is `tab`, and is a non-negative integer.
For tab scope the worker first probes the tab (`page_tab` rules) and refuses
the mint itself with `PermissionNeeded` when the tab has no page access; the
broker is not called.

Reply:

```json
{"ok": true, "code": "tb-...", "grant": "g-...", "scope": "tab",
 "profile": "<uuid>", "tab": 7, "expires_at": 1763140000.0}
```

`expires_at` is Unix seconds. `tab` appears only for tab scope. On `ok` the
worker records `{grant, scope, tab}` in its local grant table (see "Worker
grant table"). The worker never stores the code.

Errors (`{"ok": false, "error": <name>}`):

| Error | Cause |
|---|---|
| `Unauthorized` | Peer or Chrome-host verification failed (existing) |
| `InvalidMint` | Malformed frame, unknown scope, bad or misplaced `tab` |
| `NotEnrolled` | No enrollment or pairing matches the presented credential |
| `BrowserDisconnected` | Tab scope and no paired browser for the profile |
| `GrantLimit` | Tab scope: a live tab grant already holds this tab incarnation; carries `"limit": "tab"`. No limit applies to the number of grants across tabs or profiles |
| `AuditUnavailable` | The audit line could not be written; no code exists |

## Grant list and revoke from the popup — `enrollment.sock` only

Sender: the extension worker, for the popup. Each request carries the profile
credential and sees only that profile's codes and grants. The reply shapes
match the operator ops.

| Request | Reply |
|---|---|
| `{"op": "grants", "profile", "email", "key"}` | `{"ok": true, "grants": [entry, ...]}` |
| `{"op": "revoke_grant", "profile", "email", "key", "grant": "g-..."}` | `{"ok": true, "revoked": 1}` |
| `{"op": "revoke_grants", "profile", "email", "key"}` | `{"ok": true, "revoked": <count>}` |

`entry` is the operator entry (see "Operator ops"). Errors: `Unauthorized`
(verification failed), `InvalidRequest` (malformed frame), `NotEnrolled` (the
credential matches nothing), `UnknownGrant` (no such grant in this profile; a
grant of another profile answers the same). Revoke ends a grant at once with
`reason: revoked`, writes the best-effort `revoke` line, and sends
`grant_end` to the worker.

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
wins. A tab-scoped redeem sends the browser `page_tab` for the granted tab and
binds only when the tab still exists in the same pairing generation. Page
access is not required to redeem. Mint does not probe through the broker. On
any refusal the broker writes the error frame and closes the connection.

| Error | Cause |
|---|---|
| `InvalidRequest` | Malformed frame |
| `CodeRefused` | Wrong, already used, expired, or being redeemed by another connection. The agent cannot tell which; the audit log can |
| `RedeemLocked` | Lockout is active; carries `retry_after_s` (whole seconds, rounded up). A correct code is refused too |
| other browser refusal | Tab scope: the `page_tab` probe failed for another reason, such as `ProfileMismatch`; the code stays redeemable |
| `TabClosed` | Tab scope: the tab incarnation ended before redeem, or the probe answered `TabNotFound` or `UnknownGrant`. The code is consumed and never bound to another tab |
| `BrowserDisconnected` | Profile scope: no paired browser; the code stays redeemable until it expires |
| `GrantLimit` | Tab scope: another live tab grant holds the tab; the code stays redeemable |
| `AuditUnavailable` | The audit line could not be written; the code stays redeemable |

Lockout: `CodeRefused` outcomes count as failures. `FAIL_LIMIT` failures
within `FAIL_WINDOW` lock redeem for every connection. Attempts during a lock
are refused without counting, so a lock always expires. Each lock that follows
within `LOCK_MAX` of the previous one doubles, up to `LOCK_MAX`; a successful
redeem, or `LOCK_MAX` without a lock, resets the length. Mint is never locked.

## Operations on a code session — `agent.sock`

A code session uses only the ops below. A workflow session (`open`) keeps
`claim`, `snapshot`, `click` and `release` unchanged and refuses the ops below
with `InvalidRequest`. A code session refuses `claim`, `snapshot` and `click`
with `InvalidRequest`. Competing-claim refusal (`TabClaimed`) applies between
code sessions and workflow sessions alike: one controller per tab at claim
time, held by a lease.

Field types: `tab` is a non-negative integer. `lease`, `request_id` and `ref`
are non-empty strings of at most 8192 characters (the existing `string` rule).
`text` and `value` are strings of 0 to 8192 characters. `url` is a string of 1 to 8192
characters. Every request carries exactly the fields listed.

| op | Request fields | Success reply fields |
|---|---|---|
| `tabs` | `op` | `tabs`: list of tab entries |
| `select` | `op`, `tab` | `lease` str, `tab` int, `access` bool, `document` str or null, `origin` str or null, `title` str or null |
| `page_read` | `op`, `lease` | `tab` int, `document` str, `document_changed` bool, `origin` str, `url` str, `title` str, `text` str, `elements` list, `truncated` bool |
| `page_click` | `op`, `lease`, `request_id`, `ref` | `tab` int, `document` str |
| `page_type` | `op`, `lease`, `request_id`, `ref`, `text` | `tab` int, `document` str |
| `page_select` | `op`, `lease`, `request_id`, `ref`, `value` | `tab` int, `document` str, `changed` bool, `value` str |
| `page_navigate` | `op`, `lease`, `request_id`, `url` | `tab` int, `access` bool, `document` str or null, `origin` str or null, `settled` bool |
| `release` | `op`, `lease` | none beyond `ok` (existing) |

Every success reply also carries `"ok": true`.

A tab entry is `{"tab": int, "window": int, "index": int, "active": bool,
"access": bool, "origin": str | null, "title": str | null}`. `origin`
(`scheme://host[:port]`) and `title` (at most 200 characters) are present only
when `access` is true; otherwise both are null. The broker passes on only
these seven keys of each entry the worker returns, and for a tab grant only
the entry of the granted tab.

`select` takes a lease on the tab for this session (existing `Claim`, lease
`TTL`, one holder per tab incarnation) and does not focus or activate the tab.
Selecting a tab this session already holds returns the same lease. A session
may hold leases on several tabs. `select` on a tab without page access
succeeds with `access: false`, so the agent can still navigate it.

`page_click`, `page_type` and `page_navigate` keep the existing journal: a
repeat `request_id` with an identical frame returns the stored reply without
running again; a repeat with a different frame answers `Conflict`; 4096
entries answer `SessionFull`. Only a reply from an operation the worker ran,
`ok` or `OutcomeUnknown`, is journaled. A refusal is not journaled, so a retry
with the same `request_id` runs again. The journal is read right after frame
validation and before lease validation, so a journaled `OutcomeUnknown` whose
lease was dropped still answers from the journal and sends nothing to the
browser.

Each success refreshes the lease deadline, as today. Code-session leases send
no `lease_show` or `lease_clear` to the browser; see "Control indicator".

Refusals on these ops (`{"ok": false, "error": <name>, ...}`):

| Error | Fields | Cause |
|---|---|---|
| `InvalidRequest` | none | Malformed frame, unknown op, or an op of the other session kind (existing) |
| `StaleLease` | none | The lease is unknown, expired, released, or of another session or generation (existing) |
| `OutOfScope` | `tab` | Tab grant: the tab is not the granted tab; or either side's scope check refused (existing name, field added) |
| `TabClaimed` | `holder`, `expires_at` | `select`: another session holds the tab (existing) |
| `TabNotFound` | `tab` | Profile grant: the tab does not exist; the grant stays |
| `PermissionNeeded` | `tab`, `origin` | `page_read`, `page_click`, `page_type`: the tab has no page access. `origin` is the origin the user must activate when the extension knows it, else null |
| `DocumentChanged` | `tab`, `access`, `document` | `page_click`, `page_type`: the tab's top document is not the one this lease last read; `document` is the current one when `access` is true, else null |
| `StaleElement` | `tab` | `ref` does not name an element of this document's latest read, or the element is no longer connected |
| `NotEditable` | `tab` | `page_type`: the element is not a supported editable target |
| `NotSelectable` | `tab` | `page_select`: the element is not an enabled single-choice `select` |
| `NoSuchOption` | `tab` | `page_select`: no enabled option has that value or label |
| `InvalidUrl` | none | `page_navigate`: the URL fails the URL floor |
| `ProfileMismatch` | none | The browser's signed-in account changed (existing) |
| `BrowserDisconnected` | none | `tabs`, `select`, `page_read`: no reply from the browser, or the worker could not finish the read (existing). A read changes nothing, so a retry is safe |
| `OutcomeUnknown` | none | `page_click`, `page_type`, `page_navigate`: the browser did not answer after the command was sent, or the worker could not finish it (existing). Journaled; a retry with the same `request_id` returns it |
| `Conflict`, `SessionFull` | none | Journal refusals (existing) |
| `GrantEnded` | `reason` | The grant is gone. The broker closes the connection after this frame (existing) |
| `AuditUnavailable` | none | The admitted audit line could not be written; nothing was sent to the browser (existing) |

Every refusal in this table changes no page, grant, lease, document record or
journal entry, apart from its audit lines and the lease and grant effects
named in "Per-operation checks". `OutcomeUnknown` is the one reply that may
follow a page change; the agent reads the page to learn the outcome.

`GrantEnded.reason` is one of `revoked`, `tab_closed` (the granted tab
answered `TabNotFound`), `browser_gone` (the profile's pairing generation
ended or was replaced, or the worker no longer knows the grant), `unenrolled`
(the enrollment or pairing was removed or replaced), `idle` (no operation for
`GRANT_IDLE`), `audit` (an operation's result line could not be written). An
idle grant ends at its own next operation, or earlier at the next mint, redeem
or operator op, whichever comes first; once ended it no longer counts toward
`MAX_GRANTS_PER_TAB` or appears in a `grants` list, and its agent's next call
answers `GrantEnded` with `reason: idle`. A grant also ends silently when its
socket closes. When a grant ends its leases are dropped and the broker sends
`grant_end` to the worker, best effort, without delaying the agent's reply
(see "All-https access" for `last` and the reconnect list). A click, type or
navigate in flight when its grant ends answers `OutcomeUnknown`; any other op
in flight answers `GrantEnded`.

Result-line audit failure: the operation already ran in the browser, but its
result line could not be written. The broker ends the grant with
`reason: audit` and never releases an unrecorded result: `tabs`, `select`,
`page_read` and `release` answer `GrantEnded` with `reason: audit` and the
broker closes the connection; `page_click`, `page_type` and `page_navigate`
answer `OutcomeUnknown` (journaled under the `request_id`), and the session's
next call answers `GrantEnded` with `reason: audit`.

A worker error name outside the worker refusal table reaches the agent as
`BrowserRefused` with no fields.

## Operator ops — `operator.sock`

Sender: the local operator CLI, `trusty-browser`. The existing
`{"op": "revoke", "workflow"}` is unchanged.

| Request | Reply | CLI verb |
|---|---|---|
| `{"op": "grants"}` | `{"ok": true, "grants": [entry, ...]}` | `trusty-browser grants` |
| `{"op": "revoke_grant", "grant": "g-..."}` | `{"ok": true, "revoked": 1}` or `{"ok": false, "error": "UnknownGrant"}` | `trusty-browser revoke-grant g-...` |
| `{"op": "revoke_grants"}` | `{"ok": true, "revoked": <count>}` | `trusty-browser revoke-grants` |
| `{"op": "pairings"}` | `{"ok": true, "pairings": [{"profile", "email"}, ...]}` | `trusty-browser pairings` |
| `{"op": "unpair", "profile": "<uuid>"}` | `{"ok": true, "profile": "<uuid>"}` or `{"ok": false, "error": "UnknownProfile"}` | `trusty-browser unpair <uuid>` |

Operator `unpair` has the effect of the extension's `unpair` (the profile's
codes and grants end with `unenrolled`) without a credential, as the
operator `revoke` does for a workflow; it exists for a profile whose
extension is gone. The CLI validates its argument before connecting (a grant
id is `g-` and 12 lower-case hex characters; a profile is a UUID v4), prints
the reply as one JSON line, and exits 1 on `"ok": false` or when the service
is not running.

An entry is `{"grant", "scope", "profile", "tab" (tab scope only), "state":
"code" | "active", "expires_in_s" (code) or "idle_s" (active)}`. It never
carries a code or a hash. Revoke applies to an unredeemed code or a live grant.
Malformed frames answer `RevokeFailed`, as today.

## Browser hop: page commands — `service.sock` and the native port

The broker sends these commands over the existing paired connection
(`com.trusty.browser` native host, `Peer.command` framing with a fresh `id`).
The existing commands (`inspect`, `snapshot`, `click`, `lease_show`,
`lease_clear`) are unchanged and keep their `email`, `url`, `account`
expectations. Page commands carry `email` only, because a paired profile has
no site route.

Envelope:

- Request: `{"id": <str>, "op": <name>, "email": <str>, "grant": <str>, ...}`.
- Success: `{"id", "ok": true, "email", "result": {...}}`, plus top-level
  `"tab": <int>` for every command that names a tab. The broker refuses a
  reply whose `email` differs (`IdentityMismatch`) or whose `tab` differs from
  the request (`TabMismatch`), as today.
- Refusal: `{"id", "ok": false, "error": <name>, ...}`. Only the fields `tab`,
  `origin`, `access` and `document` may accompany an error. The broker relays
  them to the agent and drops any other field.
- The worker answers within 7 seconds; the broker waits 8 (existing). A
  missing answer is `BrowserDisconnected`, or `OutcomeUnknown` for
  `page_click`, `page_type` and `page_navigate`. A worker refusal named
  `BrowserDisconnected` or `OutcomeUnknown` is treated the same way, by the
  command's kind and not by the name the worker chose (see "Worker
  refusals").

| op | Extra request fields | `result` on success |
|---|---|---|
| `page_tabs` | none | `{"tabs": [tab entry, ...]}` |
| `page_tab` | `tab` | `{"access": bool, "document": str \| null, "origin": str \| null, "title": str \| null}` |
| `page_read` | `tab`, `document` (str or null: the lease's last document) | `{"document", "origin", "url", "title", "text", "elements", "truncated"}` |
| `page_click` | `tab`, `document` (str or null), `ref` | `{"document": str}` |
| `page_type` | `tab`, `document` (str or null), `ref`, `text` | `{"document": str}` |
| `page_select` | `tab`, `document` (str or null), `ref`, `value` | `{"document": str, "changed": bool, "value": str}` |
| `page_navigate` | `tab`, `url` | `{"access": bool, "document": str \| null, "origin": str \| null, "settled": bool}` |
| `grant_end` | `reason` (a `GrantEnded` reason, `closed`, or `expired` for an unredeemed code), `last` bool | `"ended"` |

`grant_end` carries no `tab`, and the broker does not wait for its reply
before answering the agent. `last` is defined in "All-https access".

Pairing acknowledgement (existing frame, field added): after a good proof the
broker answers `{"ok": true, "generation": <str>, "live": [<grant id>, ...]}`.

The broker adds `document_changed` to the agent's `page_read` reply by
comparing `result.document` with the lease's previous document.

Worker refusals. `InvalidOperation`, `ProfileMismatch`, `TabNotFound`,
`UnknownGrant` and `OutOfScope` are existing names; the others are new in this
build:

| Error | Fields | Cause |
|---|---|---|
| `InvalidOperation` | none | Unknown `op` (existing) |
| `InvalidRequest` | none | Missing, extra or mistyped field |
| `ProfileMismatch` | none | `email` differs from the signed-in account (existing) |
| `UnknownGrant` | none | `grant` is not in the worker grant table |
| `OutOfScope` | `tab` | Tab grant and `tab` differs from the grant's tab |
| `TabNotFound` | `tab` | `chrome.tabs.get(tab)` fails (existing name) |
| `PermissionNeeded` | `tab`, `origin` | The probe or the operation's injection failed for lack of page access |
| `DocumentChanged` | `tab`, `access`, `document` | `page_click`, `page_type`: the top document differs from `document`, or `document` is null |
| `StaleElement` | `tab` | See "Element addressing" |
| `NotEditable`, `NotSelectable`, `NoSuchOption` | `tab` | See "Element addressing" |
| `InvalidUrl` | none | `page_navigate`: the URL fails the URL floor |
| `OutcomeUnknown` | none | `page_click`, `page_type`, `page_navigate`: an unexpected exception, or a result that cannot show the command ran, after the page may have changed |
| `BrowserDisconnected` | none | `page_tabs`, `page_tab`, `page_read`: an unexpected exception |

The broker relays these last two without fields and by the command's kind:
for `page_click`, `page_type` and `page_navigate` either name reaches the
agent as `OutcomeUnknown` and is journaled; for any other command either name
reaches the agent as `BrowserDisconnected`, which is not journaled.

`grant_end` never refuses except with `InvalidRequest` or `ProfileMismatch`;
an unknown grant answers `"ended"`.

### Worker check order

For every page command the worker checks, in order, and stops at the first
failure: known `op`; exact fields and types; `email`; `grant` in the grant
table; scope (tab grant: `tab` equals the grant's tab); `chrome.tabs.get(tab)`
succeeds. Then it runs the command. No check reads the active tab.

### Worker grant table

The worker keeps `{grant: {scope, tab}}` in `chrome.storage.session`, so it
survives a service-worker restart and empties on a browser restart. It adds a
row on a mint reply and removes it on `grant_end`, on a popup revoke reply, on
`unpair`, when a `grants` list reply no longer contains it, and when the
pairing acknowledgement's `live` list no longer contains it. It does not
remove a tab grant when the tab closes; the next command answers
`TabNotFound`, the broker ends the grant with `tab_closed`, and `grant_end`
removes the row.

The worker also keeps `{tab: origin}` in `chrome.storage.session`: the origin
of the tab's last successful probe, or of the last `page_navigate` target on
that tab, whichever is later. `PermissionNeeded.origin` reads this map, null
when the tab has no entry. The map holds origins only, never paths.

### URL floor

`page_navigate.url` must parse as an absolute URL with scheme `https:` (any
host), or `http:` with host `127.0.0.1`; no username or password; at most 8192
characters. Every other URL, including `javascript:`, `data:`, `file:`,
`chrome:` and `about:`, answers `InvalidUrl`. The broker checks the floor
with the frame's other fields, before it sends anything to the browser, and
the worker checks it again before `tabs.update`.

## Element addressing and the read result

An element is addressed by a `ref` that a `page_read` of the same document
returned. The agent never writes a selector.

- `page_read` runs one injected function in the isolated world of the tab's
  top document. It walks the document, keeps the elements it reports in an
  isolated-world `Map` from ref to `WeakRef(element)`, and replaces any earlier
  map. The map dies with the document.
- A ref is `"<read>.<n>"`: `read` is a per-document counter that the injected
  function increments on each read, `n` the element's index in that read's
  list. Both are decimal integers; the ref matches `^[0-9]{1,9}\.[0-9]{1,6}$`.
- `page_click` and `page_type` inject into `documentIds: [document]` only. The
  injected function answers `StaleElement` when the ref's `read` is not the
  map's current counter, the ref is absent, the `WeakRef` is empty, or the
  element is not connected.

Why this scheme: it needs no page mutation (no attributes are written into the
site's DOM), no selector language, and no state outside the document, and a
ref cannot reach a different document or an element from an older read.

`page_read` result fields:

| Field | Type | Content |
|---|---|---|
| `document` | str | The top document's `documentId` from the `InjectionResult` |
| `origin` | str | `location.origin` |
| `url` | str | `location.href`, at most 2048 characters |
| `title` | str | `document.title`, at most 200 characters |
| `text` | str | `document.body.innerText`, at most 65536 characters; empty when there is no body |
| `elements` | list | At most 500 element entries, in document order |
| `truncated` | bool | True when any field or the list was cut |

An element entry is `{"ref": str, "tag": str, "role": str, "name": str}` plus,
when they apply, `"type": str` (input type), `"value": str` (at most 512
characters; never for `type=password`), `"href": str` (absolute, at most 2048
characters), `"checked": bool`, `"disabled": true`, and for a `select`
`"options"`: the first 100 options as `{"value": str, "label": str}` plus
`"selected": true` and `"disabled": true` when they apply (more than 100 sets
`truncated`). `tag` is the lower-case
tag name. `role` is the `role` attribute, else the implicit role (`link`,
`button`, `textbox`, `checkbox`, `radio`, `combobox`, `listbox`, `searchbox`,
`generic`). `name` is the first non-empty of `aria-label`, `aria-labelledby`
text, associated `<label>` text, `placeholder`, `innerText`, `title`, `alt`,
with whitespace collapsed, at most 200 characters.

Reported elements: `a[href]`, `button`, `input` other than `type=hidden`,
`select`, `textarea`, `summary`, `[contenteditable]` other than `"false"`, and
any element whose `role` is `button`, `link`, `checkbox`, `radio`, `tab`,
`menuitem`, `option`, `switch`, `textbox`, `combobox` or `searchbox`. An
element is reported only when `getClientRects().length > 0` and its computed
`visibility` is `visible`. Only the top document's light DOM is walked.

Size bound: the JSON encoding of `result` is at most 262144 bytes of UTF-8.
The injected function measures it; over the bound it shortens `text` first,
then drops elements from the end of the list, and sets `truncated: true`.

`page_click`: `scrollIntoView({block: "center", inline: "center"})`, then
dispatch bubbling `pointerdown`, `mousedown`, `pointerup`, `mouseup` at the
element's centre, `focus()` when the element is focusable, then
`element.click()`. The reply does not wait for any navigation the click
starts.

`page_type`: supported targets are `input` of type `text`, `search`, `email`,
`url`, `tel`, `password` or `number`, `textarea`, and a contenteditable
element; any other target, or a disabled or read-only one, answers
`NotEditable`. For `input` and `textarea`: `focus()`, set the value through the
prototype's native `value` setter, then dispatch bubbling `input` and
`change`. For contenteditable: `focus()`, select its contents, then
`document.execCommand("insertText", false, text)`. The text replaces the
current content; empty `text` clears it.

`page_select` (#11285): the target must be an enabled single-choice
`select`; any other answers `NotSelectable`. `value` names the first enabled
option whose value equals it, else whose label does; none answers
`NoSuchOption`. Both refusals change nothing. An option already selected
dispatches nothing and answers `changed: false`. Otherwise `focus()`, set
`selectedIndex`, then dispatch bubbling `input` and `change`. `changed`
compares the select's value before and after the events, so a page handler
that reverts the choice answers `changed: false`; `value` is the value after.

## Tab incarnation and documentId

- **Tab incarnation (broker)**: a lease is keyed by `(peer.generation, tab)`,
  as today. A tab grant binds one incarnation at mint. Any command for a
  lease whose generation is not the grant's answers `GrantEnded` with
  `browser_gone`; the broker never resends an old tab id into a new
  generation.
- **documentId (worker)**: the broker stores on each code-session lease the
  `document` of its latest `select` or `page_read` reply, null until one
  reports access, and sends it with every `page_read`, `page_click` and
  `page_type`. The broker never enforces it; the worker does.
- `page_click` and `page_type` require the stored `document` to be the tab's
  current top document. The worker injects with `documentIds: [document]`;
  when Chrome reports no such document, or `document` is null, it probes the
  top frame and answers `DocumentChanged` with the probe's `access` and
  `document`. Nothing runs in the page.
- `page_read` reads the current top document whatever its id, provided page
  access holds, and returns its `document`. The broker stores it and sets
  `document_changed` when it differs from the stored one.
- `page_navigate` ignores the stored `document`, calls `tabs.update`, waits up
  to 5 seconds for `chrome.tabs.onUpdated` to report `status: "complete"` for
  that tab, then probes. `settled` is false when the wait timed out. The
  broker stores the probe's `document` (null without access).

What the agent sees when the document changes between two calls: its next
`page_click` or `page_type` answers `DocumentChanged` and changes nothing. A
`page_read` then succeeds with `document_changed: true` and fresh refs when
the new document is on the activated origin, or answers `PermissionNeeded`
when the change crossed origins.

## Per-operation checks, audit and grant fate

Before every op the broker runs, in order: grant liveness (`check_live`:
idle, generation, enrollment or pairing still current), the admitted audit
line, frame validation, the journal (`page_click`, `page_type`,
`page_navigate`: a recorded `request_id` answers here), and lease validation.
Then the op's own scope check
below runs, then the browser command. A failure stops the sequence; later
steps do not run.

| op | Broker scope check | Worker scope check |
|---|---|---|
| `tabs` | Grant generation is the peer's; tab grant: the reply list is filtered to the grant's tab | Grant known; tab grant: list only the grant's tab. Probes each listed tab for `access`, `origin`, `title` |
| `select` | Generation; tab grant: `tab` equals the grant's tab; `TabClaimed` check | Check order, then the read-only probe |
| `page_read` | Lease valid and owned; lease generation is the grant's; tab grant: lease tab equals the grant's tab | Check order; inject into the top frame; failure is `PermissionNeeded` |
| `page_click`, `page_type` | As `page_read` | Check order; inject only into `documentIds: [document]` |
| `page_navigate` | As `page_read`, plus the URL floor | Check order, URL floor, `tabs.update` on the explicit tab id |
| `release` | Lease valid and owned (existing) | Not sent to the worker |

The probe is `executeScript({target: {tabId, frameIds: [0]}, func})` with a
function that returns `[location.origin, document.title]` and changes nothing;
`document` comes from the `InjectionResult`. A probe failure means no page
access.

Audit lines. Each op writes the fail-closed `admitted` line and then a result
line, as in phase 1. The `admitted` line is written before frame validation,
so it carries `op` and no `tab`; only `page_navigate`'s carries an `origin`,
the target host, and only when the URL passes the floor. On result lines
`tab` is the operated tab (absent for `tabs`) and `origin` is a host name
only. A refusal's result line is best effort.

| op | `op` value | `origin` on the result line | Never recorded |
|---|---|---|---|
| `tabs` | `tabs` | absent | tab titles, origins |
| `select` | `select` | host of the probed page when `access` | title |
| `page_read` | `page_read` | host of the read page | text, elements, URL path or query, title |
| `page_click` | `page_click` | host of the page | ref, element name |
| `page_type` | `page_type` | host of the page | typed text, ref, element name |
| `page_select` | `page_select` | host of the page | chosen value, ref, element name |
| `page_navigate` | `page_navigate` | host of the landed page when `access`; the admitted line carries the target host | URL path, query, fragment |
| `release` | `release` | absent | none |
| malformed | `invalid` | absent | the frame |

`outcome` on the result line is `ok` or the error name. No line carries page
text, typed text, a URL path, query or fragment, a title, a code, a lease
token, a ticket or a key.

Grant and lease fate per refusal:

| Refusal | Grant | Lease |
|---|---|---|
| `InvalidRequest`, `InvalidUrl`, `Conflict`, `SessionFull` | stays | stays |
| `PermissionNeeded`, `DocumentChanged`, `StaleElement`, `NotEditable`, `NotSelectable`, `NoSuchOption` | stays | stays |
| `StaleLease` | stays | already gone |
| `TabClaimed` | stays | none taken |
| `OutOfScope` | stays | dropped when one was named |
| `TabNotFound`, profile grant | stays | dropped |
| `TabNotFound`, tab grant | ends `tab_closed`; the agent gets `GrantEnded` | dropped |
| `UnknownGrant` from the worker | ends `browser_gone`; the agent gets `GrantEnded` | dropped |
| `ProfileMismatch`, `IdentityMismatch`, `TabMismatch` | stays until the pairing generation ends | dropped |
| `BrowserDisconnected`, `OutcomeUnknown` | ends `browser_gone` when the peer is gone, else stays | dropped |
| `AuditUnavailable` (admitted line) | stays | stays; nothing was sent |
| result line write fails | ends `audit` | dropped |

## Control indicator

Code-session leases show no page border in this build. The worker sets the
action badge to `ON` and the title to `Trusty Browser: agent access granted`
while its grant table is not empty, in addition to the existing workflow-lease
badge, and clears both when the table and the workflow leases are empty.

## Popup and worker messages

The popup (`setup.html`) sends `chrome.runtime.sendMessage({type, ...})`. The
worker accepts it only from its own `setup.html`, as today, and answers
`{ok: true, ...}` or `{ok: false, error: <str>}`. The existing types
(`activate`, `enroll`, `replace`, `cancel`, `preview`, `enable`, `unenroll`)
are unchanged. New types:

| type | Fields | Worker action | Reply on success |
|---|---|---|---|
| `code_preview` | `tab` (int or null) | Read the credential; probe `tab` by its id | `paired` bool, `email` str, `profile` str or null, `target` `{tab, access, origin, title}` or null. `paired` is true when the profile holds a pairing (false for a workflow enrollment); `profile` is the credential's profile, null when the profile holds no credential |
| `pair` | none | `pair` on `enrollment.sock` (see "pair") | `profile` str |
| `unpair` | none | `unpair`, then clear `pairing` and the grant table | `profile` str |
| `mint` | `scope` (`"tab"` or `"profile"`), `tab` (int, tab scope only) | Tab scope: probe the tab, refuse `PermissionNeeded` without access; then `mint` | the broker's mint reply fields |
| `grants` | none | `grants` with the credential; prune the grant table | `grants`: entries, each tab entry adding `origin` and `title` from a probe when the tab has access, else null |
| `revoke_grant` | `grant` str | `revoke_grant`; remove the row | `revoked` int |
| `revoke_grants` | none | `revoke_grants`; empty the table | `revoked` int |

The popup reads `chrome.tabs.query({active: true, currentWindow: true})` once,
when it loads, and sends that id as `tab`. It never queries the active tab
again. A broker error reaches the popup as `error` set to the error name.

## Popup states

| State | Shown | Controls and what they send |
|---|---|---|
| Loading | Robot, status line | none; sends `code_preview` |
| Unavailable | The reason: service unreachable, or no signed-in Chrome account | Retry sends `code_preview` |
| Not paired | "Let an agent use this browser with an activation code" above **Pair this profile**; "Connect this site's workflow to Trusty Browser" above **Enroll this site's workflow** | **Pair this profile** sends `pair`, then `code_preview`; it requests no permission. On success the status line reads "Profile paired: <email>"; on failure it shows the error |
| Choose scope | The target tab's origin and title; the grant list | **Give access to this tab** (primary, first) sends `mint` with `scope: "tab"`; **Give access to the whole profile** (secondary, second, labelled as every tab in this profile) sends `mint` with `scope: "profile"` |
| Code shown | The code in a monospace field, a countdown `m:ss` to `expires_at`, the scope, the grant list | **Copy** writes the code with `navigator.clipboard.writeText` (sends nothing); **Cancel code** sends `revoke_grant` for its grant |
| Code expired | "Code expired" | **New code** returns to Choose scope |
| Grant list (part of the two states above) | One row per entry: scope, tab origin and title when known, `waiting for agent, m:ss left` (code) or `agent connected, idle m:ss` (active) | **Disconnect** per row sends `revoke_grant`; **Disconnect all** sends `revoke_grants`. The list refreshes with `grants` every 5 seconds while the popup is open |
| Workflow enrolled | Existing activation details; the status line reads "Extension active" | Existing controls, plus Choose scope above them |

When the target tab has no page access (for example a `chrome://` page), the
tab choice is disabled with "Open a web page and click Trusty Browser on it";
the profile choice stays. A paired profile shows **Disconnect this profile**
under the details, which sends `unpair`. The popup keeps the code in memory
only; closing the popup discards it, and the unredeemed code stays listed
until it expires or is cancelled.

## MCP tools

`trusty-browser-mcp` keeps workflow mode (`--workflow` or `--credential`, with
the existing four tools `browser_claim`, `browser_snapshot`, `browser_click`,
`browser_release`) unchanged. Run with neither flag, it starts in code mode.

### Code mode

Code mode (`src/trusty_browser/agent_mcp.py`, tests in
`tests/test_agent_mcp.py`):

- Start-up reads no credential, touches no file and opens no socket. The
  server answers MCP `initialize` and `tools/list` whether the broker is
  running or not, and whether `agent.sock` exists or not. It never exits
  because the broker is down.
- `browser_connect` opens `<state>/agent.sock`, sends `redeem` as the first
  frame and keeps that one connection as the session. Every later tool call
  uses it.
- Server shutdown (stdin closed) closes the session, so the broker ends the
  grant with `closed`.
- Code mode registers the page tools below in place of `browser_claim`,
  `browser_snapshot` and `browser_click`, which exist in workflow mode only.
  Tool names are provisional.

| Tool | Parameters | Result |
|---|---|---|
| `browser_connect` | exactly one of `code_file: str` (preferred) or `code: str` | The redeem reply: `ok`, `grant`, `scope`, `profile`, `tab` (tab scope only), `idle_timeout_s`. Never the code |
| `browser_disconnect` | none | `{"ok": true, "disconnected": true}`; `disconnected` is `false` when no session was held |
| `browser_tabs` | none | The `tabs` reply: `ok`, `tabs` |
| `browser_select` | `tab: int` | The `select` reply |
| `page_read` | `lease: str` | The `page_read` reply |
| `page_click` | `lease: str`, `ref: str`, `request_id: str` | The `page_click` reply |
| `page_type` | `lease: str`, `ref: str`, `text: str`, `request_id: str` | The `page_type` reply |
| `page_select` | `lease: str`, `ref: str`, `value: str`, `request_id: str` | The `page_select` reply |
| `page_navigate` | `lease: str`, `url: str`, `request_id: str` | The `page_navigate` reply |
| `browser_release` | `lease: str` | `{"ok": true}` |

Every refusal is an MCP tool error (`isError: true`). Its text is
`Error executing tool <tool>: ` followed by one compact JSON error frame. A
broker refusal is relayed as the broker wrote it, for example
`{"ok":false,"error":"GrantLimit","limit":"tab"}` or
`{"ok":false,"error":"RedeemLocked","retry_after_s":60}`. Adapter-local
refusals carry `ok`, `error` and `message`:

| Error | When | Session after |
|---|---|---|
| `ServiceUnavailable` | `agent.sock` is missing, refuses the connection, closes, times out (30 s) or sends a bad frame, on any tool that uses the socket | None. A later `browser_connect` works once the broker is up, without restarting the server |
| `NotConnected` | Any tool other than `browser_connect` and `browser_disconnect` with no session | None |
| `AlreadyConnected` | `browser_connect` while a live session is held | Unchanged. The new code is not sent and stays redeemable |
| `InvalidRequest` | `browser_connect` with a `code` that is not a string, a `code_file` that is not a non-empty string, or both or neither of `code` and `code_file`. The message never quotes the value | Unchanged; nothing is sent |
| `CodeFileRefused` | `browser_connect` with a `code_file` the adapter refuses (see "Passing a code by file"). The message is `code_file <path>: <reason>` | Unchanged; nothing is sent |

Second-connect rule: refuse. `browser_connect` on a live session answers
`AlreadyConnected`; call `browser_disconnect` first. Before refusing, the
adapter peeks the socket without blocking; a session the broker has closed is
dropped and the connect proceeds, so a dead session never blocks a new code.

Session drop: a refused redeem (every error in "redeem"), a `GrantEnded`
reply, and any `ServiceUnavailable` drop the session. The next tool other
than `browser_connect` answers `NotConnected`, and `browser_connect` with a
new code works.

The code is never logged, written to disk, or placed in a result or error
text. The adapter has no log line of its own, and the MCP library logs only
request types.

### Passing a code by file

An activation code is a secret. Passed as `code`, it lands in the agent's
transcript. Instead, the owner saves the code to a file only they can read
and gives the agent the path; the agent calls
`browser_connect(code_file="/path/to/file")`. The adapter reads the file and
sends the same `redeem` frame that `code` sends.

```sh
(umask 077; pbpaste > ~/.trusty/code.txt)   # creates the file as 0600
```

The adapter refuses the file, with `CodeFileRefused`, when it:

- is a symlink (checked without following it);
- does not exist, is not a regular file, or cannot be read;
- is not owned by the current user;
- has any group or other permission bit set. Use mode `0600` or `0400`;
- is larger than 1024 bytes, is not UTF-8, or is empty or whitespace only.

Surrounding whitespace and a trailing newline are stripped from the code.
The refusal message names the path and the reason, never the file's content.

## Audit log

`<state>/audit.jsonl`, mode 0600, opened `O_APPEND | O_NOFOLLOW`, rotated at
5 MiB to `audit.jsonl.1` and `audit.jsonl.2` (two kept). One JSON object per
line with only these keys: `ts`, `event`, `grant`, `scope`, `profile`, `op`,
`tab`, `outcome`, `origin` (URL host only). It never carries a code, a code
hash, a ticket, a lease token, a key, page content, typed text, a title, or a
URL path, query or fragment.

Events: `mint`, `redeem` (outcome `ok`, `wrong`, `replay`, `expired`,
`revoked`, `contended`, `locked`, `tab_closed`, `browser_disconnected`,
`limit`, or the browser's refusal name when the tab probe fails), `lock`
(outcome is the lock length, for example `60s`), `pair` and `unpair` (outcome
`ok` or the error name; best-effort; written for the extension's and the
operator's `unpair`), `revoke`, `grant_end` (outcome is the reason, or
`closed`; an unredeemed code that expires writes no line), `operation` (outcome `admitted`, then a second line with
`ok` or the error name; per-op fields in "Per-operation checks").

Failure policy: mint, redeem and each operation's `admitted` line are
fail-closed — if the write fails the request is refused with
`AuditUnavailable` and nothing is created, consumed or run. A failed result
line ends the grant (`reason: audit`). Pair, unpair, revoke, lock and
grant-end lines are best-effort: a failed write never blocks them.

## Not in this build

- Any manifest change.
- Console, network or debugger access (`chrome.debugger`, devtools protocol,
  request interception).
- Trusted input events; every event is synthetic.
- Screenshots or any capture of pixels.
- A page border on code-session tabs; the badge is the indicator.
- Iframes and shadow-DOM content in `page_read`, `page_click`, `page_type`.
- Choosing an option in a `<select>`, file inputs, drag, hover, scroll and
  keyboard shortcuts.
- Focusing or activating a tab from the agent.
- Page access on a tab the user did not click when the user declined
  all-https access, and on any non-`https:` page other than the clicked one;
  such a tab answers `PermissionNeeded` for read, click and type.

## File ownership

Paths are under `python/trusty-browser/`. No file is in both columns.

| Python builder (broker and MCP shim) | JavaScript builder (worker and popup) |
|---|---|
| `src/trusty_browser/protocol.py` | `src/trusty_browser/extension/worker.js` |
| `src/trusty_browser/service.py` | `src/trusty_browser/extension/pages.js` (new: injected page functions, loaded by `importScripts`) |
| `src/trusty_browser/leases.py` | `src/trusty_browser/extension/setup.html` |
| `src/trusty_browser/grants.py` | `src/trusty_browser/extension/setup.js` |
| `src/trusty_browser/pages.py` (new: code-session ops) | `src/trusty_browser/extension/setup.css` |
| `src/trusty_browser/codes.py` | `src/trusty_browser/extension/indicator.js`; `src/trusty_browser/extension/codes.js` (new: worker grant table, all-https permission, pairing and the popup code flow, loaded by `importScripts`) |
| `src/trusty_browser/audit.py` | `src/trusty_browser/extension/indicator_content.js` (no change expected) |
| `src/trusty_browser/enrollment.py`, `src/trusty_browser/pairing.py` (new: pairing records, split for the line cap) | `src/trusty_browser/extension/content.js` (no change expected) |
| `src/trusty_browser/agent_mcp.py` | `src/trusty_browser/extension/manifest.json` (frozen: no change) |
| `src/trusty_browser/cli.py` (operator verbs); `native_host.py`, `mac_peer.py`, `vault.py`, `assets.py`, `install_host.py` (no change expected) | `tests/test_worker.mjs` |
| `tests/test_activation_codes.py` | `tests/test_popup.mjs` |
| `tests/test_service.py` | `tests/test_indicator.mjs` |
| `tests/test_agent_mcp.py` | `tests/test_content.mjs` |
| `tests/test_enrollment.py`, `tests/test_transfer.py`, `tests/test_host.py`, `tests/test_cli.py`, `tests/test_vault.py` | `tests/test_activation_dom.py` (Playwright popup test) |
| `tests/test_page_ops.py` (new), `tests/test_pairing.py` (new) | `tests/test_indicator_dom.py` |
| `tests/live_test.py`, `tests/test_live_fixture.py`, `tests/evidence/` | `tests/test_pages_dom.py` (new: Playwright test of `pages.js` functions on a static page) |
| `pyproject.toml`, `README.md` | |

`docs/activation-codes.md` belongs to neither builder. A builder that finds
the contract wrong stops and reports; the contract changes first. Each side
assumes the other implements this document exactly and tests against a fake of
the other hop: Python fakes the worker's page-command replies, JavaScript
fakes the broker's frames and the native host. `pyproject.toml` already ships
`extension/*`, so `pages.js` needs no packaging change. An end-to-end test
across both sides follows after both merge and is not in either builder's
scope.

## Tests per side

Each refusing arm is tested as a behaviour: the refusal name and fields, and
that no state advanced (no command reached the next hop, the lease, grant and
journal are unchanged, and the page did not change).

Python (broker and shim):

- Out-of-scope tab: a tab grant's `select` of another tab answers `OutOfScope`
  and sends nothing to the worker; a profile grant from another profile's
  pairing never reaches this profile's peer; `tabs` on a tab grant lists only
  its tab even when the fake worker returns more.
- Stale tab incarnation: a lease from an older generation answers
  `GrantEnded` `browser_gone`; nothing is sent to the new peer.
- Stale `documentId`: the lease's stored `document` is sent on
  `page_click`/`page_type`; a `DocumentChanged` reply is relayed with its
  fields, the lease stays, the journal holds no entry for that `request_id`,
  and a retry with the same `request_id` runs again.
- `PermissionNeeded`: relayed with `tab` and `origin`; grant and lease stay.
- Ended grant: after revoke (popup or operator), idle, unpair, `TabNotFound`
  on a tab grant, and `UnknownGrant`, the next op answers `GrantEnded` with
  the right reason, the connection closes, and `grant_end` is sent to the
  fake worker.
- Malformed request: extra or missing fields, a workflow op on a code session,
  a code op on a workflow session, a bad URL, and a non-string `text` each
  answer `InvalidRequest` or `InvalidUrl` with nothing sent to the worker.
- Pairing: `pair` is idempotent; conflicting `pair` answers
  `EnrollmentConflict`; mint with a pairing credential succeeds and with a
  wrong key answers `NotEnrolled`; `unpair` ends the profile's grants.
- Popup list and revoke: a credential sees only its profile's entries; another
  profile's grant id answers `UnknownGrant`.
- Audit: no line carries typed text, read text, a title, a URL path or query,
  or a ref; the admitted-line failure answers `AuditUnavailable` with nothing
  sent; a result-line failure ends the grant with `audit` and releases no
  result.
- Unfinished worker command: a worker `OutcomeUnknown` or
  `BrowserDisconnected` to a click, type or navigate reaches the agent as
  `OutcomeUnknown`, and a retry with the same `request_id` returns it while
  the fake worker receives the command once; the same names on a read reach
  the agent as `BrowserDisconnected` and a retry runs again.
- Last-grant signal: `grant_end` reaches the worker on every end route with
  `last` true only for the profile's final code or grant; the pairing
  acknowledgement lists exactly the live codes and grants.
- Shim (`tests/test_agent_mcp.py`): start-up and `tools/list` with no
  broker and no socket; tools before `browser_connect` answer `NotConnected`;
  a second connect answers `AlreadyConnected` and spends no code; each redeem
  refusal is relayed and leaves the shim reconnectable; `GrantEnded` and
  broker loss drop the session; disconnect and shutdown end the grant; the
  code never reaches a result, the server's stderr or a file; a non-string
  code is refused without being quoted; one end-to-end run connects, lists
  tabs, selects, reads, types, clicks, navigates, releases and disconnects.

JavaScript (worker, page functions, popup):

- Out-of-scope tab: a tab grant's command for another tab answers `OutOfScope`
  and injects nothing; no command path calls `chrome.tabs.query` with
  `active: true`.
- Stale `documentId`: `page_click`/`page_type` with an old or null `document`
  answer `DocumentChanged` with the probe's `access` and `document`, and the
  page's click and input listeners never fire.
- `PermissionNeeded`: an injection that Chrome refuses answers
  `PermissionNeeded` with `tab` and the remembered `origin` (null when none);
  mint of a tab without access is refused without contacting the broker.
- Ended grant: a command after `grant_end`, after a popup revoke, or for a
  grant id never minted answers `UnknownGrant`; the badge clears when the
  grant table empties.
- Malformed request: unknown op answers `InvalidOperation`; missing, extra or
  mistyped fields, and a ref that fails the pattern, answer `InvalidRequest`;
  a URL outside the floor answers `InvalidUrl` and `tabs.update` is not
  called; a wrong `email` answers `ProfileMismatch`.
- Page functions: a ref from an earlier read, or of a removed element, answers
  `StaleElement`; `page_type` on a button or a read-only input answers
  `NotEditable`; a password input's value is never in `elements`; a page over
  the size bound returns at most 262144 bytes with `truncated: true`.
- Navigate: same-origin navigation reports `access: true` with a new
  `document`; cross-origin reports `access: false`; a load past 5 seconds
  reports `settled: false`.
- Popup: the tab choice comes before the profile choice; the popup sends the
  tab id captured at load; the countdown reaches `Code expired`; Disconnect
  sends `revoke_grant` with the row's grant id; the workflow controls are
  hidden while a pairing exists.
- All-https access: a profile mint click requests `https://*/*` inside the
  click when it is absent and mints whether or not the user grants it; a tab
  mint click requests nothing and mints; the
  **Pair this profile** click requests nothing; the permission is removed on each
  signal of "All-https access" (`grant_end` with `last: true`, an empty
  `live` list, an empty `grants` reply, no credential, the offline check)
  and is kept on `grant_end` with `last: false`.
