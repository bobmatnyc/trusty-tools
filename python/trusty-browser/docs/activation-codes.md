# Activation: one-click Connect — broker frame contract

Status: built for issue #11239. Owner rulings (2026-11-16): pairing is one
**Connect** click in the popup, with no code, no copy-paste and no terminal
step (03:37Z); any same-user local agent can drive a connected profile, and
Connect grants the whole profile (Dpj); agents get full user parity with no
per-action gates, and the guards sit at the edges: Connect per profile, a
connected badge, instant **Disconnect**, and a full audit log (03:52Z). After
a browser restart the user clicks Connect again; nothing reconnects by itself.

The activation-code flow this document described before (mint a `tb-` code,
paste it to an agent, redeem it) is removed, and so are the workflow enroll
ops and the site adapter `content.js`. `com.trusty.browser.enroll` stays: it
is the verified channel for `pair`, `unpair`, `connect`, `grants`,
`revoke_grant` and `revoke_grants`. Every other op on it answers
`InvalidRequest`. The broker keeps grants in memory only, so a broker restart
ends every grant.

All frames use the existing length-prefixed JSON framing. Every socket still
requires a same-uid Unix peer (`verify_peer`). Requests with unknown or missing
fields are refused.

## Terms

- **Grant id**: a non-secret id such as `g-3f9a1c2b7d0e`. Operators revoke by
  this id.
- **Profile credential**: the triple `profile` (UUID v4), `email` (the
  profile's signed-in Chrome account, lower case) and `key` (64 lower-case hex
  characters) that the extension holds in `chrome.storage.local`. It comes
  from the profile's existing workflow `enrollment` when one exists, else from
  its `pairing` (see "pair"). A profile holds one credential, never both kinds.
- **Profile grant**: the standing grant Connect creates. Its scope is always
  `profile` (every tab of the profile). A profile holds at most one; a repeat
  Connect returns it. Any number of agent sessions may attach to it, and an
  agent leaving never ends it.
- **Tab incarnation**: the Chrome tab id inside one browser pairing generation
  (`peer.generation`). A navigation inside the same tab keeps the incarnation;
  a closed or replaced tab, or a browser restart, ends it. See "Tab
  incarnation and documentId".
- **Page access**: the right to inject into a tab's top document, from the
  optional `https://*/*` host permission the Connect click requests (see
  "All-https access"), or from `activeTab` on a tab where the user clicked the
  extension action.
- Constants (`codes.py`): `RECONNECT_GRACE = 45` s. Lease `TTL = 60` s
  (`leases.py`). There is no cap on the number of grants or attached agents.
- Grant lifetime (owner ruling, #11239): a grant has no idle timeout. It
  lasts until the browser closes, the user disconnects it, the operator
  revokes it, or its pairing or enrollment goes away. The browser is
  identified by the hello's `incarnation`, which the worker keeps in
  `chrome.storage.session`: it survives a service-worker restart and is
  cleared when Chrome closes. When the native port drops, the profile's
  active grants wait `RECONNECT_GRACE` for a new hello. The same
  `incarnation` rebinds them to the new pairing generation before the
  acknowledgement's `live` list is built. A different `incarnation` ends
  them `browser_gone` at that hello, and no hello within the grace ends them
  `browser_gone` at the grace deadline, or at a later hello when no sweep ran
  first. During the grace an op answers `BrowserDisconnected` and the grant
  stays. The 45 s covers the worker's 30 s maximum reconnect backoff plus a
  margin.
- Each rebind writes a `grant_reuse` audit line (outcome `ok`), between the
  grant's `connect` line and its `grant_end` line. Rebind fails closed: a
  grant with no recorded `incarnation` gets no grace and ends `browser_gone`
  when its port drops, and a rebind whose `grant_reuse` line cannot be
  written ends the grant `browser_gone`. A revoke during the grace ends the
  grant at once; the reconnecting worker's `live` list no longer holds it.
  A grant that ends this way also drops its leases, so the badge and the
  grant list never report an agent on an ended grant.

## Build constraints

- Manifest permissions stay exactly: `storage`, `nativeMessaging`, `identity`,
  `identity.email`, `alarms`, `scripting`, `activeTab`, `downloads` (#11289),
  `debugger` (#11294). No `host_permissions`. The optional `https://*/*` stays
  declared. Adding a permission requires one re-approval per install (owner
  ruling 2026-11-15). The manifest declares no content scripts (the site
  adapter is removed); the worker unregisters any `site-*` script a profile
  registered under the removed site enrollment.
- Page code runs only through `chrome.scripting.executeScript` in the default
  `ISOLATED` world, on the top frame, of a tab the worker names by explicit id.
  `page_read` and `page_find` also inject into every frame (`allFrames`) and
  into permitted cross-origin child documents by `documentIds` (#11301); no
  `webNavigation` permission is used. No operation looks up "the active tab".
- `page_click` input is trusted (`isTrusted: true`, #11294):
  the worker attaches `chrome.debugger` to the named tab for the one command,
  sends DevTools Protocol `Input.*` events, and detaches. Chrome shows its
  "started debugging this browser" bar while attached. `chrome.debugger` is
  used for input and native dialogs only: no console, network or `Runtime`
  access beyond one 100 ms in-page wait for late dialogs. Every other event
  dispatched into a page, including typing and key presses, is synthetic
  (`isTrusted: false`).
- `chrome.tabs.update(tabId, {url})` navigates any tab; it needs no permission
  and is confined only by the checks in this contract.
- Without the `tabs` permission, `chrome.tabs.query` returns no `url` or
  `title` for a tab without page access. The worker learns a tab's origin and
  title only through a successful injected probe.

## All-https access

The popup calls `chrome.permissions.request({origins: ["https://*/*"]})`
inside the **Connect** click handler, before it sends `connect`, whenever the
permission is absent. It needs the user gesture, which the service worker
does not have. Declining stops the click: nothing is sent, and the popup
reads "Connect needs access to your sites. Click Connect and allow it."

Removal. The worker calls `chrome.permissions.remove({origins:
["https://*/*"]})` when it learns that the profile holds no live grant and no
connect is in flight. It learns that from exactly these signals:

1. `grant_end` with `"last": true` (see "Browser hop"). The broker sends
   `grant_end` on every end route: Disconnect or operator revoke
   (`revoked`), tab closed (`tab_closed`), worker `UnknownGrant` or a browser
   that did not return within `RECONNECT_GRACE` (`browser_gone`), result-line
   audit failure (`audit`), and unpair or unenroll (`unenrolled`). `last` is
   true when, after this end, the profile holds no live grant.
2. The `service.sock` pairing acknowledgement, which carries
   `"live": [grant id, ...]`: every live grant of the profile. The worker
   prunes its grant table to this list on every (re)connect, and removes the
   permission when the list is empty. This covers every `grant_end` the
   worker missed while its port was down, including a broker restart.
3. A `grants` reply whose list is empty. The worker sends `grants` itself when
   a connect failed after the permission was granted in its click, and when a
   `grant_end` with `last: true` leaves a row in its table.
4. No credential. A profile with no `pairing` and no `enrollment` can hold no
   grant, so the worker removes the permission when `unpair` finishes and
   whenever `pair` or `unpair` fails and leaves no credential.
5. The offline check. While the native port is down the broker can send
   nothing, yet then holds nothing of the profile once `RECONNECT_GRACE` has
   passed: the port's loss ends every grant of its pairing generation after
   the grace. The worker records when the port dropped, moves that time
   forward on each successful connect, and empties its table and removes the
   permission when it has gone `OFFLINE_QUIET` without an acknowledgement.
   `OFFLINE_QUIET = RECONNECT_GRACE + 60` s, which is 105 s; the 60 s margin
   covers a probe in flight (`PAGE_WAIT`, 8 s) and the worker's one-minute
   reconnect alarm, on which the check runs. The worker writes the figure as
   `QUIET_MS = 105000` in `extension/codes.js`;
   `tests/test_activation_codes.py`
   `test_the_worker_offline_quiet_matches_the_reconnect_grace` fails when
   either side changes without the other.
6. A failed worker `grants` call while the native port is up. The worker
   keeps its table and removes the permission when that table is empty and no
   connect is in flight.
7. Disconnect. `revoke_grants` ends every grant of the profile, so the worker
   empties its table and removes the permission on its `ok` reply.

The broker's deadline callback (one, on its own event loop) is set to the
earliest reconnect-grace deadline and ends what is due when it fires. An
active grant whose browser is connected has no deadline. It is re-armed after
every port loss or hello and every sweep, and cancelled with its in-flight
sweep when the broker closes. A sweep that raises is logged and still re-arms.

## pair — `enrollment.sock` only

Sender: the extension worker, inside a **Connect** click on a profile that
has no credential. Same peer and Chrome-host verification as before.

| Request | Reply |
|---|---|
| `{"op": "pair", "profile": "<uuid>", "email": "<str>", "key": "<64 hex>"}` | `{"ok": true, "profile": "<uuid>"}` |
| `{"op": "unpair", "profile": "<uuid>", "email": "<str>", "key": "<64 hex>"}` | `{"ok": true, "profile": "<uuid>"}` |

The worker generates `profile` (`crypto.randomUUID()`) and `key` (32 random
bytes, hex), writes them to `chrome.storage.local` as `pairing_pending`, sends
`pair`, retries with the same fields after an interruption, and on `ok` moves
them to `pairing` and reconnects its native port. The broker stores the key
sealed with the vault (purpose `browser:<profile>`). `pair` with fields
identical to a stored pairing answers `ok` and changes nothing. A definite
refusal drops `pairing_pending`, so the next Connect starts fresh.

**Newest same-email pairing wins** (owner ruling Dqw 2026-11-16). A profile
that lost its extension storage pairs again under a new `profile` and `key`.
When no workflow route matches the profile or email and exactly one pairing
matches the email under another profile id, `pair` replaces that row in the
same `BEGIN IMMEDIATE` transaction. The broker then detaches the old profile:
its grants end with `GrantEnded.reason: unenrolled`, its agent sessions close
and its `service.sock` peer is dropped. The best-effort `pair_replaced` audit
line carries `profile` (new) and `replaced` (old). A matching route, or a
stored profile with another email or key, still refuses `EnrollmentConflict`.

`unpair` removes the pairing and ends every grant of the profile with
`GrantEnded.reason: unenrolled`, closes their agent sessions and closes the
profile's `service.sock` connection. The popup's **Forget this profile** link
sends the worker `unpair`. **Disconnect** never does, so the pairing stays and
the next Connect needs no new `pair`. The operator's `unpair` remains. A retried `unpair` after a lost
`ok` reply answers `Unauthorized`, which the worker treats as done.

| Error | Cause |
|---|---|
| `Unauthorized` | Peer or Chrome-host verification failed, or `unpair` presented a credential that matches no pairing |
| `InvalidEnrollment` | Malformed frame |
| `EnrollmentConflict` | A workflow route matches the profile or email, or the profile holds a pairing with another email or key |

On `enrollment.sock` an `Unauthorized` or `EnrollmentConflict` reply also
carries `reason`, the code from "Refusal log", and the popup shows it.
`service.sock` and `agent.sock` replies never carry `reason`.

## connect — `enrollment.sock` only

Sender: the extension worker, in the **Connect** click, after `pair` when the
profile had no credential. The worker first waits up to 5 s for the native
port's acknowledgement, so the broker sees the profile's browser; without one
it answers `BrowserDisconnected` and sends nothing. When this click paired the
profile, a `BrowserDisconnected` answer is retried exactly once, after 750 ms,
while the native port reconnects (owner ruling Dqw 2026-11-16).

Request: `{"op": "connect", "profile", "email", "key"}`.

Reply: `{"ok": true, "grant": "g-...", "scope": "profile", "profile": "<uuid>"}`.

The broker creates the profile grant bound to the current peer's generation
and `incarnation`, or returns the profile's live grant unchanged. It writes
the fail-closed `connect` audit line before the grant exists. On `ok` the
worker records `{grant: {scope: "profile", tab: null}}` in its grant table; a
reply that is not ok, or a malformed one, records nothing, and a grant the
table cannot store is revoked again with `revoke_grant`.

| Error | Cause |
|---|---|
| `InvalidRequest` | Malformed frame, or an extra field |
| `NotEnrolled` | No enrollment or pairing matches the credential |
| `BrowserDisconnected` | The profile's browser is not connected to the broker |
| `AuditUnavailable` | The `connect` line could not be written; no grant exists |

## Grant list and Disconnect — `enrollment.sock` only

Each request carries the profile credential and sees only that profile's
grants. The reply shapes match the operator ops.

| Request | Reply |
|---|---|
| `{"op": "grants", "profile", "email", "key"}` | `{"ok": true, "grants": [entry, ...]}` |
| `{"op": "revoke_grant", "profile", "email", "key", "grant": "g-..."}` | `{"ok": true, "revoked": 1}` |
| `{"op": "revoke_grants", "profile", "email", "key"}` | `{"ok": true, "revoked": <count>}` |

**Disconnect** sends `revoke_grants`. Revoke ends each grant at once with
`reason: revoked` and sends `grant_end` to the worker, then writes the
best-effort `revoke` line: a failed audit write never keeps a grant alive.
Every attached agent's next call answers `GrantEnded` and its connection
closes. Errors: `Unauthorized`, `InvalidRequest`, `NotEnrolled`,
`UnknownGrant` (no such grant in this profile).

## attach and profiles — `agent.sock`, first frame only

Sender: any same-uid agent process. No code is needed: the user's Connect
made the grant.

| Request | Reply |
|---|---|
| `{"op": "profiles"}` | `{"ok": true, "profiles": [{"profile", "email", "grant"}, ...]}`; the broker then closes the connection |
| `{"op": "attach"}` | `{"ok": true, "grant", "scope": "profile", "profile", "email"}` when exactly one profile is connected |
| `{"op": "attach", "profile": "<uuid>"}` | the same reply, for that profile |

The attached connection is the agent's session; every later frame uses it.
Choosing among several connected profiles is explicit: `attach` without
`profile` never picks one. `attach` writes the fail-closed `attach` audit
line; a session that closes writes the best-effort `detach` line and leaves
the grant standing. The workflow ticket `{"op": "open", "workflow", "ticket"}` is removed and answers `InvalidRequest`, like `redeem`.

| Error | Cause |
|---|---|
| `InvalidRequest` | Malformed frame, including the removed `redeem` |
| `NotConnected` | No connected profile, or none matches `profile` |
| `ProfileChoiceRequired` | Several profiles are connected and `profile` is absent; carries `profiles` |
| `AuditUnavailable` | The `attach` line could not be written; no session exists |

## Operations on a session — `agent.sock`

An attached session uses only the ops below. A workflow session (`open`) keeps
`claim`, `snapshot`, `click` and `release` unchanged and refuses the ops below
with `InvalidRequest`. A code session refuses `claim`, `snapshot` and `click`
with `InvalidRequest`. Competing-claim refusal (`TabClaimed`) applies between
code sessions and workflow sessions alike: one controller per tab at claim
time, held by a lease.

Field types: `tab` is a non-negative integer. `lease`, `request_id` and `ref`
are non-empty strings of at most 8192 characters (the existing `string` rule).
`text` and `value` are strings of 0 to 8192 characters. `url` is a string of 1 to 8192
characters. Every request carries exactly the fields listed, except the optional
`dialog` of `page_click` (#11294): `"accept"` or `"dismiss"` (the default),
the answer to a confirm, prompt or beforeunload dialog the click opens. An
alert is always acknowledged. When a dialog opened, the reply adds `dialog`:
`{"type": "alert" | "confirm" | "prompt" | "beforeunload", "message": str (at
most 1024 characters), "accepted": bool}` for the last one. For `page_press`
(#11286), `ref` may also be null (the focused element); `key` is one of
`Enter`, `Tab`, `Escape`, `Backspace`, `Delete`, `ArrowUp`, `ArrowDown`,
`ArrowLeft`, `ArrowRight`, `Home`, `End`, `PageUp`, `PageDown`, `Space`, or a
single code point whose Unicode category is not C (control, format) or Z
(separator), the space character excepted; `modifiers` is a list of distinct
values from `Shift`, `Control`, `Alt`, `Meta`, possibly empty. Any other value
answers `InvalidRequest`.

| op | Request fields | Success reply fields |
|---|---|---|
| `tabs` | `op` | `tabs`: list of tab entries |
| `select` | `op`, `tab` | `lease` str, `tab` int, `access` bool, `document` str or null, `origin` str or null, `title` str or null |
| `page_read` | `op`, `lease`; optional `scope_ref` str or null, `max_chars` int 1000-250000 (default 50000), `viewport_only` bool, `include_text` bool (both default false) | `tab` int, `document` str, `document_changed` bool, `origin` str, `url` str, `title` str, `text` str, `elements` list, `truncated` bool; with cross-origin child frames also `frames` list and `denied_frames` list (#11301) |
| `page_click` | `op`, `lease`, `request_id`, `ref`, optional `dialog` (`accept` or `dismiss`, default `dismiss`) | `tab` int, `document` str, `dialog` when one opened |
| `page_hover` | `op`, `lease`, `request_id`, `ref` | `tab` int, `document` str |
| `page_type` | `op`, `lease`, `request_id`, `ref`, `text` | `tab` int, `document` str; when a `maxlength` cut the text also `truncated` true, `typed` int, `accepted` int (#11352) |
| `page_scroll` | `op`, `lease`, `request_id`, `to`, `ref`, `wait_ms` | `tab` int, `document` str, `scroll_y` int, `scroll_height` int, `viewport_height` int, `grew` bool, `at_end` bool |
| `page_press` | `op`, `lease`, `request_id`, `ref` (str or null), `key`, `modifiers` | `tab` int, `document` str; `truncated`, `typed`, `accepted` as for `page_type` |
| `page_select` | `op`, `lease`, `request_id`, `ref`, `value` | `tab` int, `document` str, `changed` bool, `value` str |
| `page_drag` | `op`, `lease`, `request_id`, `ref`, `target` | `tab` int, `document` str, `dropped` bool |
| `page_navigate` | `op`, `lease`, `request_id`, `url` | `tab` int, `access` bool, `document` str or null, `origin` str or null, `settled` bool |
| `page_upload` | `op`, `lease`, `request_id`, `ref`, `files` | `tab` int, `document` str |
| `page_downloads` | `op`, `lease` | `tab` int, `origin` str, `downloads`: list of download entries |
| `page_find` | `op`, `lease`; optional `role`, `name`, `text` (str of 1-200 characters or null; at least one set), `within_ref` str or null, `limit` int 1-50 (default 10) | `tab` int, `document` str, `document_changed` bool, `origin` str, `matches` list, `truncated` bool; `frames` and `denied_frames` as `page_read` (#11301) |
| `page_back`, `page_forward`, `page_reload` | `op`, `lease`, `request_id` | As `page_navigate` |
| `release` | `op`, `lease` | none beyond `ok` (existing) |
| `switch` | `op`, `lease` | `tab` int, `window` int, `focused` bool |

`files` (#11289) is a list of 1 to 10 `{"name", "type", "data"}` objects:
`name` is 1-255 characters with no `/` or `\`, `type` a MIME type of at most
255 characters, `data` standard base64. The decoded total is at most 512 KiB;
anything else is `InvalidRequest`. `page_upload` is mutating and journaled
like `page_type`; the journal keeps a SHA-256 of each `data`, never the bytes.
A download entry is `{"id": int, "path": str, "url": str, "mime": str,
"state": str, "bytes": number, "total": number, "error": str or null,
"started": str}`; `state` is Chrome's `in_progress`, `complete` or
`interrupted`, and `path` is the local file Chrome writes.

Every success reply also carries `"ok": true`.

A tab entry is `{"tab": int, "window": int, "index": int, "active": bool,
"access": bool, "origin": str | null, "title": str | null}`. `origin`
(`scheme://host[:port]`) and `title` (at most 200 characters) are present only
when `access` is true; otherwise both are null. The broker passes on only
these seven keys of each entry the worker returns, and for a tab grant only
the entry of the granted tab.

`select` takes a lease on the tab for this session (existing `Claim`, lease
`TTL`, one holder per tab incarnation) and does not focus or activate the tab.
Selecting a tab this session already holds returns the same lease.
`switch` (#11293) makes a leased tab the active tab of its window and focuses
that window. It needs this session's own lease, so a tab another session holds
stays unreachable (`select` answers `TabClaimed`). Focus is browser-wide state:
a tab grant answers `OutOfScope` with the lease's `tab`, even for its own tab,
and keeps the lease. `focused` is false when Chrome refused the window focus.
`switch` is idempotent and not journaled; a lost reply is
`BrowserDisconnected`. A session
may hold leases on several tabs. `select` on a tab without page access
succeeds with `access: false`, so the agent can still navigate it.

`page_click`, `page_type`, `page_press`, `page_scroll`, `page_navigate`, `page_back`,
`page_forward` and `page_reload` keep the existing journal: a
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
| `PermissionNeeded` | `tab`, `origin` | `page_read`, `page_click`, `page_type`, `page_press`: the tab has no page access. `origin` is the origin the user must activate when the extension knows it, else null |
| `DocumentChanged` | `tab`, `access`, `document` | `page_click`, `page_type`, `page_press`: the tab's top document is not the one this lease last read; `document` is the current one when `access` is true, else null |
| `NavigationPending` | `tab` | `page_read`: the tab is still loading a navigation 3 s after the read arrived, so the new document has not committed. The lease stays; read again (#11354) |
| `StaleElement` | `tab` | `ref` names no element of this document's registry, the element is no longer connected (the page removed or replaced it), or its role or name changed (#11355). Call `page_read` and use its refs |
| `NotEditable` | `tab` | `page_type`: the element is not a supported editable target |
| `NotSelectable` | `tab` | `page_select`: the element is not an enabled single-choice `select` |
| `NoSuchOption` | `tab` | `page_select`: no enabled option has that value or label |
| `Obscured` | `tab` | `page_click`: another element covers the element's centre; nothing was clicked |
| `DebuggerBusy` | `tab` | `page_click`: another debugger (such as open DevTools) holds the tab; nothing ran |
| `NotDraggable` | `tab` | `page_drag`: `target` is `ref` itself or lies inside it |
| `InvalidUrl` | none | `page_navigate`: the URL fails the URL floor |
| `NoHistory` | `tab` | `page_back`, `page_forward`: the tab has no history entry in that direction; nothing changed and the lease stays |
| `ProfileMismatch` | none | The browser's signed-in account changed (existing) |
| `BrowserDisconnected` | none | `tabs`, `select`, `page_read`: no reply from the browser, or the worker could not finish the read (existing). A read changes nothing, so a retry is safe |
| `OutcomeUnknown` | none | `page_click`, `page_type`, `page_press`, `page_scroll`, `page_navigate`: the browser did not answer after the command was sent, or the worker could not finish it (existing). Journaled; a retry with the same `request_id` returns it |
| `Conflict`, `SessionFull` | none | Journal refusals (existing) |
| `GrantEnded` | `reason` | The grant is gone. The broker closes the connection after this frame (existing) |
| `AuditUnavailable` | none | The admitted audit line could not be written; nothing was sent to the browser (existing) |

Every refusal in this table changes no page, grant, lease, document record or
journal entry, apart from its audit lines and the lease and grant effects
named in "Per-operation checks". `OutcomeUnknown` is the one reply that may
follow a page change; the agent reads the page to learn the outcome.

`GrantEnded.reason` is one of `revoked`, `tab_closed` (the granted tab
answered `TabNotFound`), `browser_gone` (the browser closed or restarted:
a hello with a new `incarnation`, or no hello within `RECONNECT_GRACE`; or
the worker no longer knows the grant), `unenrolled` (the enrollment or
pairing was removed or replaced), `audit` (an operation's result line could
not be written). No grant ends for idleness. An ended grant no longer counts
toward `MAX_GRANTS_PER_TAB` or appears in a `grants` list. A grant also ends
silently when its socket closes. When a grant ends its leases are dropped and the broker sends
`grant_end` to the worker, best effort, without delaying the agent's reply
(see "All-https access" for `last` and the reconnect list). A click, type or
navigate in flight when its grant ends answers `OutcomeUnknown`; any other op
in flight answers `GrantEnded`.

Result-line audit failure: the operation already ran in the browser, but its
result line could not be written. The broker ends the grant with
`reason: audit` and never releases an unrecorded result: `tabs`, `select`,
`page_read` and `release` answer `GrantEnded` with `reason: audit` and the
broker closes the connection; `page_click`, `page_type`, `page_press` and `page_navigate`
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
grants end with `unenrolled`) without a credential, as the
operator `revoke` does for a workflow; it exists for a profile whose
extension is gone. The CLI validates its argument before connecting (a grant
id is `g-` and 12 lower-case hex characters; a profile is a UUID v4), prints
the reply as one JSON line, and exits 1 on `"ok": false` or when the service
is not running.

An entry is `{"grant", "scope": "profile", "profile", "state": "active",
"agents" (attached sessions), "idle_s"}`; `idle_s` is informational and no
grant ends for idleness. It never carries a key. Malformed frames answer
`RevokeFailed`, as today.

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
  `page_click`, `page_type`, `page_press` and `page_navigate`. A worker refusal named
  `BrowserDisconnected` or `OutcomeUnknown` is treated the same way, by the
  command's kind and not by the name the worker chose (see "Worker
  refusals").

| op | Extra request fields | `result` on success |
|---|---|---|
| `page_tabs` | none | `{"tabs": [tab entry, ...]}` |
| `page_tab` | `tab` | `{"access": bool, "document": str \| null, "origin": str \| null, "title": str \| null}` |
| `page_read` | `tab`, `document` (str or null: the lease's last document), `scope_ref`, `max_chars`, `viewport_only`, `include_text` (the broker fills defaults) | `{"document", "origin", "url", "title", "text", "elements", "truncated"}`, plus both of `frames` and `denied_frames` or neither (#11301) |
| `page_click` | `tab`, `document` (str or null), `ref`, `dialog` | `{"document": str}`, plus `"dialog"` when one opened |
| `page_hover` | `tab`, `document` (str or null), `ref` | `{"document": str}` |
| `page_type` | `tab`, `document` (str or null), `ref`, `text` | `{"document": str}`, plus `truncated`, `typed`, `accepted` when a `maxlength` cut the text |
| `page_scroll` | `tab`, `document` (str or null), `to`, `ref` (str or null), `wait_ms` | `{"document": str}` plus `scroll_y` int, `scroll_height` int, `viewport_height` int, `grew` bool, `at_end` bool |
| `page_press` | `tab`, `document` (str or null), `ref` (str or null), `key`, `modifiers` | `{"document": str}`, plus `truncated`, `typed`, `accepted` when a `maxlength` cut the text |
| `page_select` | `tab`, `document` (str or null), `ref`, `value` | `{"document": str, "changed": bool, "value": str}` |
| `page_drag` | `tab`, `document` (str or null), `ref`, `target` | `{"document": str, "dropped": bool}` |
| `page_navigate` | `tab`, `url` | `{"access": bool, "document": str \| null, "origin": str \| null, "settled": bool}` |
| `page_upload` | `tab`, `document` (str or null), `ref`, `files` | `{"document": str}` |
| `page_downloads` | `tab` | `{"origin": str, "downloads": [download entry, ...]}` |
| `page_find` | `tab`, `document`, `role`, `name`, `text`, `within_ref`, `limit` (the broker fills defaults) | `{"document", "origin", "matches", "truncated"}`, plus both of `frames` and `denied_frames` or neither (#11301) |
| `page_back`, `page_forward`, `page_reload` | `tab` | As `page_navigate` |
| `page_switch` | `tab` | `{"window": int, "focused": bool}` |
| `grant_end` | `reason` (a `GrantEnded` reason, or `closed`), `last` bool | `"ended"` |

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
| `DocumentChanged` | `tab`, `access`, `document` | `page_click`, `page_type`, `page_press`: the top document differs from `document`, or `document` is null |
| `StaleElement` | `tab` | See "Element addressing" |
| `NotEditable`, `NotSelectable`, `NoSuchOption`, `NotDraggable` | `tab` | See "Element addressing" |
| `Obscured` | `tab` | See "Element addressing" |
| `DebuggerBusy` | `tab` | `chrome.debugger.attach` failed while the lease's document is current |
| `InvalidUrl` | none | `page_navigate`: the URL fails the URL floor |
| `NoHistory` | `tab` | `page_back`, `page_forward`: `tabs.goBack` or `tabs.goForward` rejected on an existing tab |
| `OutcomeUnknown` | none | `page_click`, `page_type`, `page_press`, `page_scroll`, `page_navigate`, `page_back`, `page_forward`, `page_reload`: an unexpected exception, or a result that cannot show the command ran, after the page may have changed |
| `BrowserDisconnected` | none | `page_tabs`, `page_tab`, `page_read`: an unexpected exception |

The broker relays these last two without fields and by the command's kind:
for `page_click`, `page_type`, `page_press` and `page_navigate` either name reaches the
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
row on a connect reply and removes it on `grant_end`, on a popup revoke reply, on
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

- `page_read` runs one injected function (`pageInspect`, in
  `extension/pages_read.js`) in the isolated world of the tab's top document.
  It walks the document and keeps an isolated-world registry: a `Map` from ref
  to `WeakRef(element)` and the element's signature, and a `WeakMap` from
  element to ref. The registry dies with the document.
- Refs are stable (#11355): an element keeps its ref across reads while its
  signature holds. The signature is the element's role and accessible name;
  for an `input`, `textarea`, `select` or contenteditable element the name
  leaves out the element's own text, so typing into a field keeps its ref. An
  element whose signature changed gets a new ref on the next read, and its old
  ref is dropped.
- A ref is `"<epoch>.<n>"`: `epoch` is a random integer drawn when the
  registry is created, `n` a per-registry counter. Both are decimal integers;
  the ref matches `^[0-9]{1,9}\.[0-9]{1,6}$`. The random epoch keeps a ref from
  an earlier registry from naming an element of a new one.
- `page_click`, `page_type`, `page_press`, `page_select`, `page_drag`, `page_upload`
  and `page_scroll` inject into `documentIds: [document]` only. The injected
  function answers `StaleElement` when the ref is absent from the registry,
  the `WeakRef` is empty, the element is no longer connected to the top
  document (through every shadow host and frame element above it, #11290), or
  its signature no longer matches. A ref is never re-resolved to a replacement node, even one
  with the same role and name.

Why this scheme: it needs no page mutation (no attributes are written into the
site's DOM), no selector language, and no state outside the document; a ref
cannot reach a different document, and a ref whose element changed its role
or name answers `StaleElement` instead of acting on it.

`page_read` result fields:

| Field | Type | Content |
|---|---|---|
| `document` | str | The top document's `documentId` from the `InjectionResult` |
| `origin` | str | `location.origin` |
| `url` | str | `location.href`, at most 2048 characters |
| `title` | str | `document.title`, at most 200 characters |
| `text` | str | With `include_text`, the scope's `innerText` (`document.body` without `scope_ref`), at most 65536 characters; else empty |
| `elements` | list | At most 500 element entries, in document order |
| `truncated` | bool | True when any field or the list was cut |

An element entry is `{"ref": str, "tag": str, "role": str, "name": str}` plus,
when they apply, `"type": str` (input type), `"value": str` (at most 512
characters; never for `type=password`), `"href": str` (absolute, at most 2048
characters), `"checked": bool`, `"disabled": true`, `"within": str`, and for a
`select` `"options"`: the first 100 options as `{"value": str, "label": str}`
plus `"selected": true` and `"disabled": true` when they apply (more than 100
sets `truncated`). `within` is the hop path from the top document's light DOM
to the element, one `iframe` or `shadow` per hop joined by `/`
(`"iframe/shadow"`); it is absent for an element in the top light DOM. `tag`
is the lower-case
tag name. `role` is the `role` attribute, else the implicit role (`link`,
`button`, `textbox`, `checkbox`, `radio`, `combobox`, `listbox`, `searchbox`,
`generic`). `name` is the first non-empty of `aria-label`, `aria-labelledby`
text (ids resolved in the element's own document or shadow root), associated `<label>` text, `placeholder`, `innerText`, `title`, `alt`,
with whitespace collapsed, at most 200 characters.

Reported elements: `a[href]`, `button`, `input` other than `type=hidden`,
`select`, `textarea`, `summary`, `[contenteditable]` other than `"false"`, and
any element whose `role` is `button`, `link`, `checkbox`, `radio`, `tab`,
`menuitem`, `option`, `switch`, `textbox`, `combobox` or `searchbox`. An
element is reported only when `getClientRects().length > 0` and its computed
`visibility` is `visible`. The walk is in document order and descends into
each open shadow root (after its host) and each visible same-origin
`iframe` or `frame` (#11290). Closed shadow roots and cross-origin frames
are not walked: page script cannot reach them. Refs keep the same
`<epoch>.<n>` form; the registry holds the element itself, wherever it lives.
`scope_ref` and `within_ref` descend the same way, so a container's ref
reaches into its open shadow roots and same-origin frames. Viewport distance
for an element inside a frame is measured in top-document coordinates (the
frame elements' offsets are added up the chain).

Read controls (#11356): `scope_ref` reads only that element and its subtree;
a ref that does not resolve (see refs above) answers `StaleElement`.
`viewport_only` keeps only elements whose box overlaps the viewport.
`include_text` adds `text`. `max_chars` bounds the result.

Size bound: the JSON encoding of `result` is at most `max_chars` and at most
262144 bytes of UTF-8, both measured as UTF-8 bytes. Elements fill the bound
in order of distance from the viewport (0 for an element on screen, nearest
first, document order on ties) and the list returns in document order; `text`
takes what is left. A cut field, element or list sets `truncated: true`.

### Cross-origin child frames (#11301)

`page_read` and `page_find` without `scope_ref` / `within_ref` also read
cross-origin child frames, read-only, when both hold:

- the extension holds Chrome host permission for the child's own origin:
  `chrome.permissions.contains({origins: ["<scheme>://<host>/*"]})`. The
  optional `https://*/*` grant covers every child. `activeTab` gives no child
  access;
- the tab is within the grant's scope (the `OutOfScope` rule above refuses
  first, before any frame is touched).

Each frame is judged on its own origin. A grandchild does not inherit its
parent's grant. The worker finds frames with one
`executeScript({target: {tabId, allFrames: true}, func: pageFrames})`: a
frame is a root when it is the top frame or its parent is cross-origin (a
root's walk already covers its same-origin descendants), and each root lists
the declared `src` origins of the cross-origin frames it embeds. It then reads
each permitted root child with `documentIds: [documentId]`. Click, type and
the other actions do not reach child frames (#11318).

With any child frame read or denied, the result adds:

| Field | Type | Content |
|---|---|---|
| `frames` | list | `{"id": int, "origin": str, "url": str, "document": str}` per frame read; the top document is first with `id` 0; `id` is Chrome's `frameId`. At most 9 (the top and 8 children) |
| `denied_frames` | list | `{"origin": str, "reason": str}` per withheld origin, at most 20, with no child content. `reason` is `no_permission`, `opaque` (no http(s) origin), `frame_limit` (more than 8 permitted children) or `navigated` (the child changed document between discovery and read) |

Every element (or match) then carries `"frame": int`, the `id` of its
`frames` entry. The top-level `document`, `origin`, `url`, `title` and `text`
stay the top document's. A child's ref is frame-scoped:
`"<frameKey>:<epoch>.<n>"` (`^[0-9]{1,9}:[0-9]{1,9}\.[0-9]{1,6}$`), where
`frameKey` is the frame's `id`; a match's `in.ref` is scoped the same way.
The worker records frameKey to `documentId` at read time. A frame-scoped
`scope_ref` or `within_ref` reads that child document only, after the
permission check again; once the frame navigated (its `documentId` changed),
or for a frame key no read recorded, it answers `StaleElement`. A
frame-scoped ref is not an action `ref`: `page_click` and the other actions
refuse it as `InvalidRequest`.

Child entries carry no viewport distance: they keep document order and
`viewport_only` does not filter them. `include_text` reads the top
document's text only. Bounds: the top document reads with half of the
budget, each permitted child with an equal share of the other half; the
merged result keeps at most 500 elements (`limit` matches) and its JSON
encoding stays within `max_chars` (262144 - 512 bytes for `page_find`),
cutting text first and then entries from the end, children first. Any cut
sets `truncated`.

`page_find` (#11357): the same injected function in find mode, run like
`page_read`. It matches the reported elements (see above) under `within_ref`
(its descendants only; a ref that does not resolve answers `StaleElement`):
`role` exactly, `name` as a case-insensitive substring of the accessible name,
and `text` as a case-insensitive substring of the element's own `innerText` or
its container's. It returns at most `limit` matches in document order, each an
element entry plus `in`: the nearest `list` (`ul`, `ol`, `role=list`), `row`
(`tr`, `role=row`), `dialog`, `form`, `navigation` or `main` ancestor as
`{"role", "name", "ref"}`, or null. The container's name is its
`aria-label` or `aria-labelledby` text, else for a row its own text, else its
first heading, `legend` or `caption`. Its `ref` works as `within_ref` and
`scope_ref`. `truncated` is true when more elements matched.

`page_click` runs in this order (#11294): probe the tab, and answer
`DocumentChanged` without attaching when the worker lacks host access or the
top document is not the lease's; attach `chrome.debugger` to the tab and enable
`Page` dialog events; inject `pageTarget` into the lease's document; send
trusted `Input.*` events at the point it returns; wait 100 ms in the page for a
dialog a handler opens late; detach. A native dialog is answered with
`Page.handleJavaScriptDialog` by the request's `dialog`. The attach comes
before targeting because Chrome's debugging bar resizes the viewport.

Before every `Input.*` event the worker probes again and stops with
`DocumentChanged` when the top document or its origin is no longer the one
probed at the start. A dialog answer Chrome rejects answers
`OutcomeUnknown`, never a reply claiming the answer. The whole command is bounded
at 5 s; past the bound it answers
`OutcomeUnknown`. Every path detaches.

`page_click`: `pageTarget` resolves the ref as above (`StaleElement` when it
does not resolve), scrolls the element to the viewport centre
(`scrollIntoView({block: "center", inline: "center", behavior: "instant"})`)
and answers `Obscured`
when `elementFromPoint` at its centre is neither the element nor inside it.
The worker then sends `mouseMoved`, `mousePressed` and `mouseReleased` (left
button, one click) at the centre, so the page sees trusted `pointerdown`,
`mousedown`, `pointerup`, `mouseup` and `click` and gains user activation.
The reply does not wait for any navigation the click starts.

`page_scroll` (#11291): `to` is `down` or `up` (90% of the viewport), `top`,
`bottom`, or `element`; `ref` is a string exactly when `to` is `element`, else
null. `wait_ms` (0 to 5000) bounds the wait for added nodes; the wait ends 150 ms
after they stop. `grew` is true when nodes were added and the document grew;
`at_end` is true when the viewport reaches the document bottom. An infinite
feed is read by repeating `to: bottom` until `grew` is false, then `page_read`.
The scroll moves the document's scrolling element only, not inner scroll
containers; `to: element` reaches an element inside one.
`page_hover` (#11287): `scrollIntoView` as `page_click`, then move the
synthetic pointer from the element this document last hovered to the target:
bubbling `pointerout` and `mouseout` on the old element, non-bubbling
`pointerleave` and `mouseleave` on each old ancestor the target is not inside
(innermost first), bubbling `pointerover` and `mouseover` on the target,
non-bubbling `pointerenter` and `mouseenter` on each new ancestor (outermost
first), then bubbling `pointermove` and `mousemove`, all at the target's
centre. Hovering the same element again sends only the two moves. It clicks
nothing and changes no focus. The events are untrusted, so CSS `:hover` does
not match; a page that reveals content only through `:hover` needs trusted
input (#11294). It is journaled like `page_click`.

`page_type`: supported targets are `input` of type `text`, `search`, `email`,
`url`, `tel`, `password` or `number`, `textarea`, and a contenteditable
element; any other target, or a disabled or read-only one, answers
`NotEditable`. The text replaces the current content; empty `text` clears it.
The worker injects `pageKeys(ref, "type", text)`: `focus()`, select the
content, then one key press (below) per code point of `text` (#11286). `\n`,
`\r` and `\r\n` press `Enter`, so a newline in a single-line field submits its
form; `\t` presses `Tab` and later characters go to the newly focused element.
An upper-case ASCII letter is pressed with `Shift`. A field's text is changed
through the prototype's native `value` setter with bubbling `beforeinput` and
`input` (`inputType` `insertText` or `deleteContent*`); a contenteditable
element uses its own document's `execCommand("insertText")`; `input` is
composed, so it leaves a shadow root, as a native one does (#11290). When the typing ends, every
field it changed gets a bubbling `change`.

`page_press` (#11286): the worker injects `pageKeys(ref, "press", {key,
modifiers})`. A non-null `ref` resolves as for `page_click` (`StaleElement`) and
is focused; a null `ref` presses on `document.activeElement`, or `body`. One
press dispatches bubbling, cancelable `KeyboardEvent`s with `key`, `code`,
`keyCode`, `which` and the four modifier flags: a `keydown` per modifier in the
order `Control`, `Alt`, `Shift`, `Meta`; `keydown` for the key; `keypress` for a
printable key or `Enter` when neither `Control` nor `Meta` is held; the default
action, unless a listener cancelled `keydown` or `keypress`; `keyup`; then the
modifier `keyup`s in reverse. Each event goes to the element focused at that
moment, so `Tab`'s `keyup` reaches the newly focused element. Events are
synthetic (`isTrusted: false`), so the default action is emulated:

| Key | Default action |
|---|---|
| printable, `Space` (no `Control`/`Meta`) | Insert into an editable target; `Space` on a button, checkbox, radio or summary clicks it after `keyup`; otherwise `Space` scrolls a page (up with `Shift`) |
| `Control`+`a` or `Meta`+`a` | Select all of the field, the editing host, or the document |
| `Enter` | Newline in a `textarea`; paragraph (line break with `Shift`) in contenteditable; implicit form submission from an `input` (click the form's default button, else `requestSubmit()` when at most one field blocks it); click a button, link or summary |
| `Tab` | Move focus in sequential order (positive `tabindex` ascending, then document order; disabled, hidden, inert, `tabindex=-1` and unchecked radios of a group skipped), backward with `Shift`, wrapping at the ends; a changed field gets `change` first; a text field it lands on is selected |
| `Backspace`, `Delete` | Delete the selection, else one character before or after the caret |
| arrows, `Home`, `End`, `PageUp`, `PageDown` | Move the caret in a field or contenteditable (extend the selection with `Shift`); otherwise scroll the page |
| `Escape`, other chords | Events only |

`page_upload` (#11289): the target must be an enabled `<input type=file>`,
with `multiple` set when more than one file is given; anything else answers
`NotEditable` and changes nothing. The worker builds a `File` per entry, sets
them through a `DataTransfer` on `input.files`, then dispatches bubbling
`input` and `change`. A file input's read entry carries `files` (the held
names, never its `C:\fakepath` value), and `multiple` and `accept` when set.

`page_downloads` (#11289): needs page access (`PermissionNeeded` otherwise)
and the manifest's `downloads` permission (`InvalidOperation` without it).
It lists the 20 newest Chrome downloads whose referrer, URL or final URL has
the tab's current origin; a `blob:` URL counts as its creator's origin. A
`data:` URL download from a `no-referrer` page is not attributed to any tab.

`page_select` (#11285): the target must be an enabled single-choice
`select`; any other answers `NotSelectable`. `value` names the first enabled
option whose value equals it, else whose label does; none answers
`NoSuchOption`. Both refusals change nothing. An option already selected
dispatches nothing and answers `changed: false`. Otherwise `focus()`, set
`selectedIndex`, then dispatch bubbling `input` and `change`. `changed`
compares the select's value before and after the events, so a page handler
that reverts the choice answers `changed: false`; `value` is the value after.

`page_drag` (#11288): `ref` and `target` are refs of one document; either may
sit inside an open shadow root or a same-origin frame. A `target` that is
`ref` or lies inside it answers `NotDraggable`. Each element is scrolled to
the centre and the events use its centre point. On the source:
`pointerdown`, `mousedown`, then `dragstart`. A cancelled `dragstart` ends the
drag with `dropped: false` and no further events. Otherwise `drag` on the
source, then `dragenter` and `dragover` on the target. When the target
cancels `dragover` (the HTML drop-target rule), `drop` fires on it and
`dropped` is `true`; otherwise `dragleave` fires and `dropped` is `false`.
`dragend` fires on the source last. All drag events share one `DataTransfer`
with `effectAllowed` `all`, so data the page sets in `dragstart` is readable
in `drop`. The events are synthetic (`isTrusted` false), so a page that
ignores untrusted drag events does not react.

Reads also report `[draggable=true]` elements (with `"draggable": true`),
`[aria-dropeffect]` other than `none`, and `[dropzone]`. When the document,
its open shadow roots or its same-origin frames hold a `[draggable=true]`
element, elements whose `id` or `class` contains `drop` (case-insensitive),
`[data-drop]` and `[data-dropzone]` are reported too, so a bare-`div` drop
zone has a ref.

## Tab incarnation and documentId

- **Tab incarnation (broker)**: a lease is keyed by `(peer.generation, tab)`,
  as today. A tab-scoped row (kept in pages code as a guard; Connect never creates one) binds one incarnation. Any command for a
  lease whose generation is not the grant's answers `GrantEnded` with
  `browser_gone`; the broker never resends an old tab id into a new
  browser incarnation. A reconnect of the same incarnation (a
  service-worker restart) moves the grant to the new generation; leases
  taken before it were dropped with the old peer.
- **documentId (worker)**: the broker stores on each code-session lease the
  `document` of its latest `select` or `page_read` reply, null until one
  reports access, and sends it with every `page_read`, `page_click`,
  `page_type` and `page_press`. The broker never enforces it; the worker does.
- `page_click`, `page_type` and `page_press` require the stored `document` to be the tab's
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
- `page_back`, `page_forward` and `page_reload` (#11292) ignore the stored
  `document`, call `tabs.goBack`, `tabs.goForward` or
  `tabs.reload(tabId, {bypassCache: false})` on the explicit tab id, then wait
  and probe as `page_navigate` does. They need no page access, no manifest
  change and no extension reload. A rejected `goBack`/`goForward` on a tab
  that still exists answers `NoHistory`; a rejected `reload` answers
  `OutcomeUnknown`.

What the agent sees when the document changes between two calls: its next
`page_click`, `page_type` or `page_press` answers `DocumentChanged` and changes nothing. A
`page_read` then succeeds with `document_changed: true` and fresh refs when
the new document is on the activated origin, or answers `PermissionNeeded`
when the change crossed origins.

## Per-operation checks, audit and grant fate

Before every op the broker runs, in order: grant liveness (`check_live`:
generation, with `BrowserDisconnected` inside the reconnect grace, and
enrollment or pairing still current), the admitted audit
line, frame validation, the journal (`page_click`, `page_type`,
`page_press`, `page_navigate`: a recorded `request_id` answers here), and lease validation.
Then the op's own scope check
below runs, then the browser command. A failure stops the sequence; later
steps do not run.

| op | Broker scope check | Worker scope check |
|---|---|---|
| `tabs` | Grant generation is the peer's; tab grant: the reply list is filtered to the grant's tab | Grant known; tab grant: list only the grant's tab. Probes each listed tab for `access`, `origin`, `title` |
| `select` | Generation; tab grant: `tab` equals the grant's tab; `TabClaimed` check | Check order, then the read-only probe |
| `page_read` | Lease valid and owned; lease generation is the grant's; tab grant: lease tab equals the grant's tab | Check order; inject into the top frame, plus permitted cross-origin child frames (#11301); failure is `PermissionNeeded` |
| `page_find` | As `page_read` | As `page_read` |
| `page_click`, `page_type`, `page_press` | As `page_read` | Check order; inject only into `documentIds: [document]` |
| `page_navigate` | As `page_read`, plus the URL floor | Check order, URL floor, `tabs.update` on the explicit tab id |
| `page_back`, `page_forward`, `page_reload` | As `page_read` | Check order, `tabs.goBack` / `tabs.goForward` / `tabs.reload` on the explicit tab id |
| `release` | Lease valid and owned (existing) | Not sent to the worker |
| `switch` | As `page_read`; tab grant: `OutOfScope` | Check order; tab grant: `OutOfScope`; `tabs.update(tab, {active: true})`, then `windows.update(window, {focused: true})` |

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
| `page_read` | `page_read` | host of the read page; `frames` lists the hosts of the child frames read (#11301) | text, elements, URL path or query, title |
| `page_find` | `page_find` | host of the read page; `frames` as `page_read` | matches and their names |
| `page_click` | `page_click` | host of the page | ref, element name |
| `page_hover` | `page_hover` | host of the page | ref, element name |
| `page_type` | `page_type` | host of the page | typed text, ref, element name |
| `page_scroll` | `page_scroll` | host of the page | ref, scroll position |
| `page_press` | `page_press` | host of the page | key, modifiers, ref, element name |
| `page_select` | `page_select` | host of the page | chosen value, ref, element name |
| `page_drag` | `page_drag` | host of the page | refs, element names |
| `page_navigate` | `page_navigate` | host of the landed page when `access`; the admitted line carries the target host | URL path, query, fragment |
| `page_upload` | `page_upload` | host of the page | file names, types and bytes |
| `page_downloads` | `page_downloads` | host of the page | download paths and URLs |
| `page_back`, `page_forward`, `page_reload` | the op name | host of the landed page when `access` | URL path, query, fragment |
| `release` | `release` | absent | none |
| `switch` | `switch` | host of the leased page when known | title |
| malformed | `invalid` | absent | the frame |

`outcome` on the result line is `ok` or the error name. No line carries page
text, typed text, a URL path, query or fragment, a title, a code, a lease
token, a ticket or a key.

Grant and lease fate per refusal:

| Refusal | Grant | Lease |
|---|---|---|
| `InvalidRequest`, `InvalidUrl`, `Conflict`, `SessionFull` | stays | stays |
| `PermissionNeeded`, `DocumentChanged`, `StaleElement`, `NotEditable`, `NotSelectable`, `NoSuchOption`, `Obscured`, `DebuggerBusy`, `NoHistory`, `NotDraggable` | stays | stays |
| `StaleLease` | stays | already gone |
| `TabClaimed` | stays | none taken |
| `OutOfScope` | stays | dropped when one was named |
| `TabNotFound`, profile grant | stays | dropped |
| `TabNotFound`, tab grant | ends `tab_closed`; the agent gets `GrantEnded` | dropped |
| `UnknownGrant` from the worker | ends `browser_gone`; the agent gets `GrantEnded` | dropped |
| `ProfileMismatch`, `IdentityMismatch`, `TabMismatch` | stays until the pairing generation ends | dropped |
| `BrowserDisconnected`, `OutcomeUnknown` | stays; a lost peer starts the reconnect grace (see "Grant lifetime") | dropped |
| `AuditUnavailable` (admitted line) | stays | stays; nothing was sent |
| result line write fails | ends `audit` | dropped |

## Control indicator

Code-session leases show no page border in this build. The worker sets the
action badge to `ON` and the title to `Trusty Browser: agent access granted`
while its grant table is not empty, in addition to the existing workflow-lease
badge, and clears both when the table and the workflow leases are empty.

## Popup and worker messages

The popup (`setup.html`) sends `chrome.runtime.sendMessage({type, ...})`. The
worker accepts it only from its own `setup.html` and answers `{ok: true, ...}`
or `{ok: false, error: <str>, reason?: <code>}`. `reason` is the broker's
refusal code when it sent one. The removed types (`mint`, `code_preview`,
`activate`, `enroll`, `replace`, `cancel`, `preview`, `enable`, `unenroll`)
get no answer.

| type | Fields | Worker action | Reply on success |
|---|---|---|---|
| `status` | none | `grants` with the credential | `email`, `profile` (null with no credential), `connected` (the live grant id, or null) |
| `connect` | none | `pair` when there is no credential, wait for the port's acknowledgement, then `connect`; record the row | `grant`, `profile` |
| `grants` | none | `grants`; prune the grant table | `grants` |
| `revoke_grant` | `grant` str | `revoke_grant`; remove the row | `revoked` int |
| `revoke_grants` | none | `revoke_grants`; empty the table, remove the permission | `revoked` int |
| `pair`, `unpair` | none | see "pair" | `profile` str |

## Popup states

| State | Shown | Controls |
|---|---|---|
| Loading | Robot | none; sends `status` |
| Unavailable | The reason, for example the service is unreachable | **Retry** sends `status` |
| Not connected | "Let local agents use this Chrome profile" above **Connect** | **Connect** requests `https://*/*` when absent, then sends `connect`. Connected shows only after an `ok` reply; on failure the error shows and Connect stays |
| Connected | The **Connected** badge, the profile's email, "Local agents can use every tab in this profile." | **Disconnect** sends `revoke_grants`, keeps the pairing and returns to Not connected. The **Forget this profile** link sends `unpair` |

A failure shows the error name, then the broker's reason code in brackets
when there is one, for example `Unauthorized (host_codesign)`.

The toolbar badge reads `ON` while the worker's grant table is not empty. A
browser restart ends the grant, so the next popup shows Not connected.

## MCP tools

`trusty-browser-mcp` keeps workflow mode (`--workflow` or `--credential`, with
the existing four tools `browser_claim`, `browser_snapshot`, `browser_click`,
`browser_release`) unchanged. Run with neither flag, it starts in connect mode.

### Connect mode

Connect mode (`src/trusty_browser/agent_mcp.py`, tests in
`tests/test_agent_mcp.py`):

- Start-up reads no credential, touches no file and opens no socket. The
  server answers MCP `initialize` and `tools/list` whether the broker is
  running or not.
- `browser_connect` opens `<state>/agent.sock`, sends `attach` as the first
  frame and keeps that one connection as the session.
- `browser_disconnect` and server shutdown close the session. The profile
  stays connected for the next agent until the user clicks Disconnect.

| Tool | Parameters | Result |
|---|---|---|
| `browser_profiles` | none | The `profiles` reply: `ok`, `profiles` |
| `browser_connect` | optional `profile: str` | The `attach` reply: `ok`, `grant`, `scope`, `profile`, `email` |
| `browser_disconnect` | none | `{"ok": true, "disconnected": true}`; `false` when no session was held |
| `browser_tabs` | none | The `tabs` reply: `ok`, `tabs` |
| `browser_select` | `tab: int` | The `select` reply |
| `browser_switch` | `lease: str` | The `switch` reply |
| `page_read` | `lease: str` | The `page_read` reply |
| `page_find` | `lease: str`, `role`, `name`, `text`, `within_ref`: str or null, `limit: int` | The `page_find` reply |
| `page_click` | `lease: str`, `ref: str`, `request_id: str`, `dialog: str = "dismiss"` | The `page_click` reply |
| `page_hover` | `lease: str`, `ref: str`, `request_id: str` | The `page_hover` reply |
| `page_type` | `lease: str`, `ref: str`, `text: str`, `request_id: str` | The `page_type` reply |
| `page_scroll` | `lease: str`, `request_id: str`, `to: str` (default `down`), `ref: str` (optional), `wait_ms: int` (default 1000) | The `page_scroll` reply |
| `page_press` | `lease: str`, `key: str`, `request_id: str`, optional `ref: str` (default: the focused element), optional `modifiers: list[str]` (default `[]`) | The `page_press` reply |
| `page_select` | `lease: str`, `ref: str`, `value: str`, `request_id: str` | The `page_select` reply |
| `page_drag` | `lease: str`, `ref: str`, `target: str`, `request_id: str` | The `page_drag` reply |
| `page_navigate` | `lease: str`, `url: str`, `request_id: str` | The `page_navigate` reply |
| `page_upload` | `lease: str`, `ref: str`, `paths: list[str]`, `request_id: str` | The `page_upload` reply; see "Upload root" |
| `page_downloads` | `lease: str` | The `page_downloads` reply |
| `page_back` | `lease: str`, `request_id: str` | The `page_back` reply |
| `page_forward` | `lease: str`, `request_id: str` | The `page_forward` reply |
| `page_reload` | `lease: str`, `request_id: str` | The `page_reload` reply |
| `browser_release` | `lease: str` | `{"ok": true}` |

Every refusal is an MCP tool error (`isError: true`) whose text is
`Error executing tool <tool>: ` followed by one compact JSON error frame. A
broker refusal is relayed as written, for example
`{"ok":false,"error":"ProfileChoiceRequired","profiles":[...]}`.
Adapter-local refusals carry `ok`, `error` and `message`:

| Error | When | Session after |
|---|---|---|
| `ServiceUnavailable` | `agent.sock` is missing, refuses, closes, times out (30 s) or sends a bad frame | None. A later `browser_connect` works once the broker is up |
| `NotConnected` | Any page tool with no session | None |
| `AlreadyConnected` | `browser_connect` while a live session is held | Unchanged |
| `InvalidRequest` | `browser_connect` with an empty `profile` | Unchanged; nothing is sent |
| `UploadsDisabled`, `FileRefused` | See "Upload root" | Unchanged; nothing is sent |

Session drop: a refused `attach`, a `GrantEnded` reply, and any
`ServiceUnavailable` drop the session. After a `GrantEnded`, `NotConnected`
carries that frame's `reason` until the next `browser_connect` attempt or
`browser_disconnect`.

### Upload root

A page can ask the agent to attach a file, and a hostile page can ask for
`~/.ssh/id_rsa`, `~/.aws/credentials` or a `.env`. So `page_upload` reads only
files under one directory the owner names at start-up:

```sh
trusty-browser-mcp --upload-root ~/Uploads/agent
```

Without `--upload-root`, every `page_upload` answers `UploadsDisabled` and no
file is read. The root must be an existing directory; the server resolves it
once, at start-up, to its real path. Give a path relative to the root, or an
absolute path inside it.

The adapter opens the root, then each path component from the directory
before it, with `O_NOFOLLOW`. No symlink below the root is followed, so a link
cannot point the read outside the root, or be swapped between a check and the
read. It refuses the path, with `FileRefused`, when it:

- has a `..` component;
- is absolute and outside the root;
- is a symlink, or passes through one;
- does not exist, is not a regular file (a directory, FIFO or device), or
  cannot be read;
- is larger than 512 KiB.

Put only the files the agent may send in the root; never use your home
directory as the root.

## Audit log

`<state>/audit.jsonl`, mode 0600, opened `O_APPEND | O_NOFOLLOW`, rotated at
5 MiB to `audit.jsonl.1` and `audit.jsonl.2` (two kept). One JSON object per
line with only these keys: `ts`, `event`, `grant`, `scope`, `profile`, `op`,
`tab`, `outcome`, `origin` (URL host only), `site`, `reason`,
`email_domain`, `replaced`, `frames` (child-frame URL hosts only, #11301). It never carries a ticket, a lease token, a key, page content, typed text, a title, or a
URL path, query or fragment.

Events: `connect` (fail-closed), `attach` (fail-closed), `detach`
(best-effort), `grant_reuse`, `pair` and `unpair` (outcome `ok` or the error
name; best-effort), `pair_replaced`, `refused` (see "Refusal log"),
`revoke`, `grant_end` (outcome is the reason),
`operation` (outcome `admitted`, then a second line with `ok` or the error
name; per-op fields in "Per-operation checks").

Failure policy: `connect`, `attach` and each operation's `admitted` line are
fail-closed: if the write fails the request is refused with
`AuditUnavailable` and nothing is created or run. A failed result line ends
the grant (`reason: audit`). Pair, unpair, detach, revoke and grant-end lines
are best-effort, and a revoke ends the grant before its line is written, so a
failed write never blocks Disconnect.

### Refusal log

Owner ruling Dqw 2026-11-16. Each refusal below writes one stderr line and one
`refused` audit line through `Audit.refuse(site, reason, profile, email_domain)`.
The line holds the profile id when the request named one, and the email
domain only (the part after `@`). It never holds the email, a key, ticket,
proof or nonce. A failed write goes to stderr and the request is still refused.

| `reason` | `site` | Check |
|---|---|---|
| `peer_uid` | `service`, `agent`, `enrollment`, `operator` | `verify_peer`: the peer is another user |
| `host_not_native_host` | `enrollment` | The peer is not our native host with its exact argv, or its identity could not be read |
| `host_parent_not_chrome` | `enrollment` | The native host's parent is not Chrome Stable |
| `host_unsafe_flag` | `enrollment` | Chrome runs with a profile-store or debugging flag |
| `host_codesign` | `enrollment` | Chrome's live code signature failed |
| `host_timeout` | `enrollment` | The host check or `codesign` timed out |
| `route_unknown` | `service` | No route or pairing for the hello's profile |
| `ticket_mismatch` | `agent` | A workflow ticket does not match its route |
| `proof_mismatch` | `service` | The extension's pairing proof is wrong |
| `key_rotated` | `service` | The pairing changed while the browser computed its proof |
| `unpair_bad_credential` | `unpair` | `unpair` with a credential that matches no pairing |
| `enroll_conflict_route` | `pair` | `pair` against a workflow route |
| `enroll_conflict_key` | `pair` | `pair` of a stored profile with another email or key |
| `remove_unauthorized` | `remove` | A workflow removal whose registration does not match |
| `status_unauthorized` | `status` | A workflow status request that matches no route or revocation |

## Not in this build

- Any manifest change beyond the `downloads` (#11289) and `debugger`
  (#11294) permissions.
- Console or network access through the devtools protocol, or request
  interception. `chrome.debugger` serves `page_click` input and dialogs only.
- Trusted typing: `page_type`, `page_press` and `page_select` still use
  synthetic events (`isTrusted: false`); only `page_click` is trusted.
- Screenshots or any capture of pixels.
- A page border on code-session tabs; the badge is the indicator.
- Closed shadow roots in `page_read`, `page_click`, `page_type`.
- Actions inside cross-origin iframes: `page_click`, `page_type` and the
  other actions reach the top document and its same-origin frames only
  (#11318). `page_read` and `page_find` read them (#11301).
- Drag events the page sees as trusted.
- File inputs inside iframes or shadow DOM, and uploads over 512 KiB.
- Browser-level shortcuts (`Meta`+`t`, `Control`+`w` and the like): a
  `page_press` chord reaches the page only.
- Focusing or activating a tab from the agent.
- Page access on a tab the user did not click when the user declined
  all-https access, and on any non-`https:` page other than the clicked one;
  such a tab answers `PermissionNeeded` for read, click and type.

## File ownership

Paths are under `python/trusty-browser/`. Broker and MCP shim:
`src/trusty_browser/{protocol,service,leases,grants,pages,codes,audit,assets,pairing,agent_mcp,cli}.py`.
Worker and popup: `src/trusty_browser/extension/{worker,codes,pages,indicator,setup}.js`,
`setup.html`, `setup.css`, `manifest.json`. `content.js` and its test are
removed. `assets.py` publishes the extension into the real directory
`<state>/extension`, updated in place, and turns every old `assets/<hash>`
directory into a symlink to it (#11363); the published manifest carries the
asset stamp in `version_name`, the hello acknowledgement carries it as
`assets`, and a worker running another stamp calls `chrome.runtime.reload()`
once per stamp. The once-per-stamp guard (`reloaded_for`) lives in `chrome.storage.local`, which the reload keeps. Just before the reload the worker writes a one-shot `reload_incarnation` handoff (incarnation and time) to `chrome.storage.local`; the next startup consumes and deletes it and reuses that incarnation when it is under 60 s old, so the broker rebinds the grant and no Connect click is needed. A browser restart finds no fresh handoff and gets a new incarnation, which ends the grant.

## Tests per side

Python: `tests/test_activation_codes.py` (connect, attach, profiles,
Disconnect, grace and reuse, audit), `tests/test_page_ops.py` (page ops on an
attached session), `tests/test_pairing.py` (including the same-email
re-pair), `tests/test_refusals.py` (the refusal log and host reason codes),
`tests/test_agent_mcp.py` (the
MCP shim), `tests/test_cli.py` (operator verbs and asset publishing, including
`test_republish_reaches_a_profile_holding_an_old_hash_path`).

JavaScript: `tests/test_worker.mjs` (worker grant table, Connect, status,
Disconnect, the permission signals, asset reload, page commands),
`tests/test_indicator.mjs`. Playwright: `tests/test_activation_dom.py` (the
popup's Connect, Disconnect, Forget and reason codes), `tests/test_pages_dom.py`,
`tests/test_indicator_dom.py`.
