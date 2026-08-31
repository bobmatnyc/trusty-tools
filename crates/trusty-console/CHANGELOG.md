# Changelog — trusty-console

All notable changes to trusty-console are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.9.1] — 2026-08-31

### Changed

- on-demand tools (trusty-review, trusty-analyze) now show the same plain
  success badge every other ready tool gets — the "Installed and ready. This
  tool runs on demand — there is no daemon to start." banner is removed
  (#6416)

## [0.9.0] — 2026-08-31

### Added

- `POST /api/webhooks/{source}` — GitHub webhook ingress that verifies the HMAC
  once over the exact received bytes, writes the delivery to an fsync'd spool
  under the console data directory, and only then acknowledges (#5089 step 3,
  ADR-0034). `{source}` multiplexes `review` and `analyze`; each relays over a
  hardened Unix socket. The ordering is the point: a spool write that fails
  returns **5xx and no 202**, so GitHub keeps the delivery redeliverable, where
  both existing handlers return 202 first and downgrade every later failure to
  a log line GitHub will never retry
- a relay outcome other than an explicit `"ack": true` leaves the spool entry
  `pending` with an incremented attempt count and a durable reason. Reaching
  the target is deliberately not enough — an entry is deleted only on the
  target's own acknowledgement
- the route accepts bodies up to 25 MiB. axum's 2 MiB `DefaultBodyLimit` would
  413 a real `push` / `pull_request` delivery *before* the handler runs — no
  spool entry, no metric, no log — which is the same invisible drop arriving
  through the framework instead of the code
- `GET /api/console/metrics/webhooks` — oldest-pending-age, pending count and
  failed-attempt total as a standard `ConsoleMetricsReport`, red once the
  oldest entry passes the threshold. The scan runs on the request rather than
  from a cache, so the signal does not go quiet if the background retry sweep
  stops. A spool directory that cannot be read — including one that was opened
  and has since been removed or unmounted — is red, never an empty listing
- retries are claimed and backed off. A `ClaimSet` gives one relay per entry at
  a time, so a sweep tick landing inside the request path's own relay window
  cannot send the same delivery twice; `BackoffPolicy` spaces attempts
  exponentially (30 s doubling to a 1 h ceiling) and stops at 24 failures. The
  claim is taken on the entry's path *before* the durable write, not after —
  claiming afterwards left the entry on disk and unclaimed for the width of one
  scheduler poll, and a sweep landing there relayed a delivery the request path
  was about to relay itself
- an entry past that limit is moved to `webhook-spool/exhausted/` rather than
  deleted or left in place. It is still an unacknowledged webhook, so it is
  kept and it keeps the health signal red — but it stops being read and
  JSON-decoded by every sweep tick and every metrics request, which with no
  target listener yet is otherwise the fate of every delivery
- the health scan reads receipt times from entry filenames and decodes exactly
  one file — the oldest live entry — instead of the whole spool. `pending` and
  `exhausted` are counted separately, and `oldest_pending_*` describes the
  oldest LIVE entry: exhausted ones are permanently the oldest, so including
  them froze the diagnostics on the first poisoned delivery and a genuinely
  new failure moved nothing an operator reads. `total_failed_attempts` is
  replaced by `oldest_pending_attempts`, which costs one decode instead of one
  per entry
- spool I/O runs on `spawn_blocking`. Ingest fsyncs a file and a directory
  twice per delivery and the metrics route scans two directories per request;
  none of that belongs on a runtime worker thread
- a fresh entry is committed with `hard_link`, not `rename`, so a colliding
  path fails atomically instead of clobbering a delivery that may already have
  been acknowledged
- The webhook relay now starts its target on demand and meters what the target is holding. `webhook::spawn::TargetSupervisor` wraps the shared `UdsServiceSupervisor` and runs `ensure_running` before each relay, so `trusty-review` and `trusty-analyze` serve their sockets without being resident; set `TRUSTY_WEBHOOK_TARGET_EXTERNAL=1` to hand their lifecycle back to `tctl`. A target that will not start is still `RelayOutcome::Unreachable` — the spool entry stays pending and is never deleted. `GET /api/console/metrics/webhooks` gains `undrained` and `undrained_total`: an acknowledged delivery leaves the spool, so without them a delivery sitting unprocessed in a target's inbox would report `Ok`; it now reports `Degraded` until something consumes it, and `Error` if a target's inbox cannot be counted. Target socket and inbox paths come from `trusty_common::webhook_relay` rather than literals here.
- Serve the trusty-search dashboard at `/tools/search/`. The console embeds its own copy of the SPA and injects `window.__SEARCH_BASE__ = /api/search/`, so every API call — chat included — rides the existing reverse proxy instead of the search daemon's own HTTP origin. The Search card links to it.
- `make -C crates/trusty-console search-ui` rebuilds and re-stamps that bundle from `crates/trusty-search/ui`; `scripts/check-ui-bundle-freshness.sh trusty-console` now checks it alongside the console's own.
- `memory_connector_accepts_the_envelope_a_real_daemon_sends` replays a
  `memory.health` frame captured verbatim from a live trusty-memory 0.25.2 over
  its socket. The existing tests reply with only the two fields the connector
  deserialises, so none of them would catch the connector refusing the eight
  extra fields a real daemon sends — the shape of the #6356 recurrence (#6356)
- Trusty Console carries a robot brand identity in the Trusty Agents family: an
  operator UNIT seated at a dashboard panel, drawn on the same head geometry as
  the agents mark so the two read as one machine doing different jobs. Assets
  are `docs/design/UI/icons/trusty-console-{mark,favicon,logo,logo-reversed}.svg`.
- The header shows that mark with the "Trusty Console" wordmark and a
  `UNIT-05 · SERVICE CONSOLE` descriptor, replacing the gradient heading and the
  "Unified service dashboard" subtitle. The Foundry identity is flat, so the
  gradient is gone rather than restyled.
- The overview panel shows the mark while services are still being detected, and
  the app ships a favicon — a browser tab showed the generic page icon before.
- One mark serves both palettes: its chassis and face read from
  `--trusty-accent` and the new `--trusty-mark-face` token, so it recolors with
  the theme instead of shipping a reversed twin.
- The dashboard deletes a trusty-memory palace and a trusty-search index from their roster rows (#6360). `DELETE /api/console/memory/palaces/{id}` calls `palace_delete` over trusty-memory's Unix socket — the transport `MemoryConnector` already uses — and `DELETE /api/console/search/indexes/{id}` calls trusty-search's own `DELETE /indexes/{id}`. The console implements no deletion of its own
- Each delete is behind a confirm step that names the exact id, so one click cannot destroy a corpus. The confirm carries the daemon's own opt-in flag: `force` for a palace that still holds drawers, `delete_data` for an index whose on-disk corpus should go rather than only its registration
- A confirmed delete re-polls the owning daemon's `console_metrics` before answering, so the roster the dashboard re-fetches reflects the delete instead of a cache written up to a poll interval ago. The row is never removed client-side
- The Memory tab compacts a palace from its roster
  (`POST /api/console/memory/palaces/{id}/compact`), calling trusty-memory's own
  `palace_compact`. Two clicks, with a confirm step naming the palace, and the
  reclaimed counts reported from the daemon's answer (#6371).
- The Search tab lists trusty-search registrations whose root directory is gone
  and removes the ones an operator confirms, in one batch
  (`POST /api/console/search/prune-indexes`). The candidate list is the daemon's
  own census; a root the daemon could not check is listed and never removed. The
  batch answers one row per id, so a prune where three succeeded and one was
  refused reads as exactly that rather than as "cleaned" (#6371).
- `GET /api/console/services` carries a `lifecycle` field on every row — `"daemon"` or `"on_demand"` (#6416). A payload written before this reads as `"daemon"`, which is what it was
- The stale-registration panel can now review and settle a registration trusty-search could not check, instead of only listing it. Expanding a row shows its full path, the daemon's reason, and the registration metadata; the operator then keeps it — a no-op — or deregisters it behind a per-row confirmation that names the path. `POST /api/console/search/deregister-unjudged` settles exactly one row through the existing `search.index.delete`, passing `delete_data: false` explicitly so a change to the daemon's default cannot move it. The index data is left untouched rather than absent, and the confirmation says which: a `colocated` index keeps its corpus beside a root the daemon could not reach, so that data may well still be there, while a non-colocated index's corpus sits in trusty-search's own directory. It fails closed: `OrphanGuard::unjudged_root` re-reads the census immediately before the delete and refuses unless the daemon still declines to judge that exact root, and a refused or unanswered attempt is reported as failed rather than counted as done. The batch prune is unchanged — it still reads the census's `orphans` list alone, so nothing sweeps an uncheckable row in.
- The Search and Memory tabs show a Last Used column and sort by it — click the header to cycle newest-first, oldest-first, and back to the daemon's order. Entries with no timestamp render as an em-dash and sort last in both directions (#6424).
- Sessions tab: every session row shows its last-used date (or "never"), and a
  sort control orders every group by it; rows with no recorded activity always
  sort last (#6430).
- Sessions tab: the unknown bucket — records whose lifecycle state is missing or
  unrecognised — supports multi-select and a record-only bulk delete, behind an
  explicit confirmation that lists every session it will delete, with its
  reported status. Deletion never removes a worktree or workspace directory, a
  session that is still running is refused rather than deleted, and a failed
  deletion is reported failed rather than counted as a success (#6431).
- `POST /api/console/sessions/bulk-delete`, backed by trusty-mpm's
  `session_delete_records` MCP tool (#6431).

### Fixed

- `trusty-console service` now names the unit launchd actually has loaded. The
  label was `com.trusty.trusty-console` while the live agent is
  `com.trusty.console`, so `service status` queried a label that does not exist
  and `service install` would have bootstrapped a second dashboard daemon beside
  the running one. The value comes from `trusty_common::launchd_labels::CONSOLE`
  and the old name is recorded as a legacy alias so an upgrade evicts it (#4868)
- `service install` now evicts the old label instead of adding a second unit
  beside it. Console is one of only two services whose label value actually
  changes, so a host that ran the pre-fix installer would otherwise keep
  `com.trusty.trusty-console` loaded AND gain `com.trusty.console` — two console
  daemons on one port, the #2938 condition this issue exists to close (#4868)
- `service uninstall` removes the unit under its old label too. On a host that
  never ran the migrating install it printed "nothing to do" while leaving
  `com.trusty.trusty-console` loaded — an uninstall that uninstalled nothing
  (#4868)
- Webhook health now meters each target's quarantined deliveries alongside its held ones and reports `Error` when any exist. Quarantining removes a delivery from the held count, so without this the signal turned green at the moment a delivery was confirmed never to be processed. `METRICS_SCHEMA_VERSION` is 3 (#5192).
- the Sessions tab's auto-resume widget shows what the supervisor is actually doing (closes [#5208](https://github.com/bobmatnyc/trusty-tools/issues/5208))
  - the label and the Enable/Disable button read `desired` — the toggle's own saved value — so with no saved setting and a supervisor booted auto-resume-on (anyone who set `TRUSTY_MPM_AUTO_RESUME` or `--auto-resume` and never used the console) it read "off" beside an Enable button while the supervisor was resuming sessions
  - both now read the daemon's new `effective` field, and toggling sends the negation of what is in force rather than of what the file says
  - that case renders "on (env default)" to mark the value as coming from the supervisor's boot flag rather than a saved setting. With no saved setting the daemon infers it from its OWN environment, and the supervisor is a separate process that may not share it — a bound the tooltip states and the supervisor publishing its resolved flag on `/metrics` would close
  - an unreadable setting renders "unknown — cannot read setting" with the button disabled, instead of a confident "off"
  - the mapping moved out of the component into `src/autoResume.js` so it can be asserted directly: `node --test src/autoResume.test.js`
- `build.rs` keeps the committed `ui/dist/` bundle instead of rebuilding it on every cold build. It used to run the package manager's install and a full `vite build` unconditionally, and `vite build` empties `ui/dist/`, deleting the tracked `ui-source-hash.txt` the publish-time freshness gate reads. Freshness is decided by `scripts/check-ui-bundle-freshness.sh`, the same check `preflight-publish.sh` runs, and an unreadable answer keeps the committed bundle rather than rebuilding it. `FORCE_UI_BUILD=1` rebuilds unconditionally and re-stamps the bundle afterwards, which is what a UI change now needs. Backported from trusty-memory ([#6060](https://github.com/bobmatnyc/trusty-tools/pull/6060), [#5078](https://github.com/bobmatnyc/trusty-tools/issues/5078))
- The reverse proxy streams an upstream response instead of collecting it first. Collecting never returned for Server-Sent Events, so `/status/stream` and `/reindex/stream` delivered nothing until the 30-second request timeout fired. A request asking for `text/event-stream` also now uses a client with a read timeout in place of that whole-request deadline.
- The proxy grants the no-total-deadline client only to a response the upstream labelled `text/event-stream`. `Accept` is a caller claim, so without that check any proxied GET could hold a connection open by asking for a stream and trickling bytes. A mid-stream body failure is logged again, as it was before the switch to streaming.
- A console request to a trusty-search endpoint with no socket method answers
  `501` naming the endpoint, and a daemon that is not listening answers `502`
  with the reason. Neither reaches the dashboard as an empty success, so "the
  daemon is down" stays distinguishable from "the daemon has nothing to show".
  `POST /chat` and `POST /admin/stop` are the two endpoints the search dashboard
  calls that have no socket method yet (#6285).
- Opening one of the dashboard's Server-Sent Event streams is now bounded at 60
  seconds total. A trusty-search daemon that accepts the connection and then
  answers nothing — a full listener backlog — used to leave the browser waiting
  out the 24-hour per-frame budget for a response head; it now answers `502`
  with the reason. The dial, the request write and the first frame read share
  one deadline, so a slow-but-successful open leaves the first read what is left
  of the 60 seconds rather than a fresh 60 of its own. An established stream
  keeps the long per-frame budget, so a reindex that emits nothing for minutes
  is still not cut off. The bridge also stops reading from the socket the moment
  the browser disconnects, rather than at the next frame, which releases the
  daemon's producer on a stream that is quiet (#6285).
- A `trusty-review` binary that is on PATH but will not run (broken signature,
  truncated download, a hang) now reports `Degraded` with the reason as its
  hint, instead of `Available` (#6290). The non-zero exit was collapsed into the
  same `None` as "no version string", which also put the console at odds with
  `tctl`, whose presence probe calls the same host `ProbeFailed`.
- `trusty-console service uninstall` reports a stale LaunchAgent it could not
  clear (#6290). It read only `evict_legacy`'s evicted-label list, so a failed
  bootout or plist deletion printed nothing at all.
- The `memory.health` probe sends `params: {}` instead of omitting `params` entirely (#6356). trusty-memory binds `HealthQuery`, whose derived `Deserialize` refuses the `null` an absent `params` decodes to, so every dial answered `-32602` and the trusty-memory row read "Available — Binary found but daemon is not running" against a live daemon
- A daemon that answers `memory.health` with an error now reports `Degraded` carrying that error, rather than being indistinguishable from a daemon that answered nothing at all. An error answer still never reads as `Running` and never carries a version
- A delete the daemon skipped no longer reads as success (#6360). `DELETE /indexes/{id}` answers `200 OK` with `removed: false` for an index trusty-search never had, and `data_deleted: false` when the registration went but the bytes stayed (#3049); both surface as a failure carrying the daemon's own words, as do a JSON-RPC refusal from `palace_delete` and any answer that does not confirm the exact id
- The console's shared HTTP client no longer follows redirects (#6360). Every loopback check in the crate — the reverse proxy's `is_local_upstream` and the delete routes' reuse of it — validates only the URL it was handed, so a `307` from an upstream re-issued the request, method and body intact, at whatever host the `Location` named. The proxy now hands a redirect back to the browser and the delete routes read it as a non-2xx refusal
- The Memory tab's palace table shows real counts for every palace and a new
  Rooms column. It used to print `—` in every count cell whose row was not
  cache-resident, so on a host with 94 palaces only one showed data. A row now
  renders `—` only when the daemon says the count could not be read, and the
  badge distinguishes "counted on disk" from "unreadable" instead of "not
  loaded" (#6372)
- The headline card reads "Palaces (counted/total)" and a Total Rooms card
  joins the aggregates, matching the totals trusty-memory now sends (#6372)
- Batch prune re-checks each registration against a fresh `search.registry.orphans` census immediately before deleting it, and pins the delete to the root path that census reported. An index id is derived from its root path, so a path wiped and recreated between the census an operator confirmed and the prune that acts on it named a live index under the same id. Every re-check failure — an unreachable daemon, a census that will not parse, an id the daemon no longer calls stale — refuses that id's delete and reports why (#6380).
- The Trusty Review and Trusty Analyze cards no longer read "Binary found but daemon is not running" in amber (#6416). trusty-review lost its daemon in #6290 and trusty-analyze serves on demand since #6287/#6350, so an installed binary with nothing serving is their healthy resting state — the console was rendering the correct state as a fault, with remediation text for a daemon the operator cannot start. Both rows now read "Ready — Installed and ready. This tool runs on demand — there is no daemon to start." in the color a running daemon gets
- The Trusty Analyze card shows a version at rest. It only ever read one off a live socket, which for an idle on-demand server is never; when nothing answers, the version comes off `trusty-analyze --version`, the way the review card's has since #6290. A binary that is on PATH but will not run now reports `Degraded` with the reason, matching the review connector and `tctl`'s `ProbeFailed`
- Sessions tab: `deleted` session tombstones now render in their own group
  instead of the catch-all "other" bucket alongside genuinely-unknown records
  (#6431).

### Changed

- The webhook module's docs no longer describe `trusty-review`'s and `trusty-analyze`'s direct HTTP webhook routes as live. #5181 deleted both, so `POST /api/webhooks/{source}` is now the only HTTP webhook surface in the workspace and the only holder of the shared secret.
- The search dashboard's Svelte source now lives in this crate at `ui-search/`, and `build.rs` builds it into the committed `ui-search-dist/` bundle alongside the console's own UI. That bundle used to be a copy of a build from `crates/trusty-search/ui`, refreshed only by an explicit `make` target; nothing is copied across crates any more. The served page and its API calls are unchanged — the rebuilt bundle has the same content hashes (#6155, #6284).
- **`ReviewConnector` dials trusty-review's Unix socket instead of probing a TCP port.** It calls `review.health` and reads the version off the answer, resolving the path through `trusty_common::daemon_socket_path` — the same call the daemon binds through. The `~/.trusty-review/http_addr` read is gone with the file, and so is the `127.0.0.1:7880` fallback, which was two port moves stale and had been trusty-mpm's daemon port since #2566: a running `tm` made this report trusty-review as Running. A service card for trusty-review no longer carries a `url`, because a UDS daemon has none (ADR-0032, [#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- trusty-review is removed from the port-collision guard table — it binds no TCP port, so reserving 7891 against it would only forbid a future daemon a free port ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- The console reaches trusty-search over its Unix socket instead of loopback
  HTTP (ADR-0032). The service card dials `search.health`, the index-delete and
  batch-prune routes dial `search.index.delete`, and `/api/search/*` — the prefix
  the console-served search dashboard calls — is translated into RPC calls rather
  than reverse-proxied. The two Server-Sent Event streams the dashboard opens,
  `/status/stream` and `/indexes/{id}/reindex/stream`, are bridged from the
  daemon's RPC streams frame for frame, keep-alive comment included. Nothing now
  reads trusty-search's `http_addr` discovery file, so a stale one left by a
  pre-migration daemon can no longer forward the dashboard to whatever holds
  7878 (#6285).
- `MemoryConnector` dials `memory.health` on trusty-memory's Unix socket instead of reading `~/.trusty-memory/http_addr` and probing the port it named (#6286, ADR-0032). Nothing rewrites that dotfile any more, so a connector still reading it would report health from a permanently stale address
- `AnalyzeConnector` dials trusty-analyze's Unix socket instead of probing
  `127.0.0.1:7879` (#6287, ADR-0032), and reports `url: None` — a UDS daemon has
  no URL, so a synthesised `http://` address would be a link that cannot work.
  The pre-migration fallback to port 7879 is gone: any process holding that port
  used to make the dashboard report a trusty-analyze that was not there.
- The `analyze` row is removed from the reverse-proxy allowlist, and 7879 from
  the `known_siblings` port-collision guard — a guard naming a port nothing binds
  refuses a value that is free.
- Removed the `trusty-mpm-supervisor` 7881 row from the `known_siblings` port-collision guard; that listener is retired and the port is free (Refs #6288).
- `ReviewConnector` reports trusty-review by presence instead of dialling its
  socket (#6290). The review daemon is retired, so the old dial spent its full
  3-second budget on every detection pass and arrived at the same `Available`
  verdict presence gives immediately.
- `Running` is now unreachable for this member, which is correct: a
  per-invocation tool is installed or it is not. The webhook path is untouched —
  console still spawns `trusty-review webhook-listen` per delivery and meters
  the drain off the inbox backlog.
- The analyze service card reads `Available` as "installed and startable"
  rather than as a degradation: trusty-analyze runs on demand, so nothing
  listening is its correct resting state. The connector deliberately does not
  start it — the console polls detect, and a detector that started the service
  would keep it resident for as long as a dashboard tab was open (#6350).
- trusty-console moves to 0.8.0. `cargo-semver-checks` reports
  `inherent_method_missing` for `ReviewConnector::with_socket` against the
  published 0.7.0 baseline — removed by #6290 when the review daemon was retired
  and the connector moved to a presence check. For a `0.y.z` crate the breaking
  bump is the MINOR position, so 0.7.1 was never a legal position for it. The
  root workspace requirement moves from `0.7.0` to `0.8.0` (#6350).
- An Overview card that offers exactly one action is clickable across its whole body, not only on its "View details" button (#6370). The clickable card is a real interactive element — `role="button"`, `tabindex="0"`, and Enter/Space activation with a visible focus ring — so it works from the keyboard. Its `aria-label` replaces the name the card contents would compute, so `aria-describedby` points back at the status badge, the version and the hint — a screen reader still says a card is degraded. A card offering two or more actions keeps a discrete button per action and stays inert itself, because no single action can stand for the card
- The tab and its section header now read "MPM Sessions" rather than "Sessions", which read as a generic label beside Search, Memory, Analyze and Review. The route, the tab id and the API fields are unchanged
- `GET /api/console/services` returns the roster sorted by liveness — running, then degraded, then installed-but-stopped, then absent — with `all_connectors()` registration order as the stable tiebreak. The dashboard renders that order, so the services with something to show lead the grid instead of whichever connector was registered first
- Deleting a search index from the dashboard now deletes its on-disk data by default, and keeping the data is the explicit opt-out (owner ruling, #6422).
  - The per-row confirm still says "This cannot be undone." and still needs a second click; what changed is that its `delete_data` checkbox starts TICKED, labelled "Delete the on-disk data too — untick to deregister only and keep the corpus". A palace delete is unaffected: `force` widens what a delete may destroy and stays opt-in, so its box still starts unticked.
  - The stale-registration prune panel starts the same way. Its confirm sentence already named the fate of the data either way; now the default it names is deletion.
  - `DELETE /api/console/search/indexes/{id}` and `POST /api/console/search/prune-indexes` both read an absent `delete_data` as `true`. The UI sends the value explicitly regardless.

### Removed

- The `memory` proxy row. `/api/memory/*` resolved a base URL from an `http_addr` file trusty-memory no longer writes but which is still on disk from before the migration, so the row could only forward to whatever now holds 7070. Deleted for the same reason the `analyze` row was (#6287), not kept inert like `review`'s
- trusty-memory's `7070` row in the known-sibling port-collision table. It binds no TCP port since ADR-0032, so reserving 7070 against it would only forbid a future daemon a free port

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

## [0.5.0] — 2026-07-21

### Changed

- **UI tokens now CI-enforced against the canonical Foundry source** (refs [#3486](https://github.com/bobmatnyc/trusty-tools/issues/3486)): flipped from the `scripts/check_token_drift.mjs` allowlist to ENFORCED. The `token-drift` CI job now compares `ui/src/theme.css`'s plain-CSS `--trusty-*: #hex` values directly (case-insensitively) to `docs/design/UI/design-system/tokens.css` on every push/PR. Enforcement is over the intersection of tokens both files define, so this crate's console-only extension tokens (`--trusty-status-degraded`, `--trusty-status-absent`) are ignored; a hand-edit that drifts a shared token from canonical fails the build.

### Added

- **Known-sibling port guard extended to `trusty-mpm`'s supervisor metrics
  listener (7881) and `trusty-code`'s new default (7882) (#3364).**
  `default_port_does_not_collide_with_known_siblings` now also rejects a
  future `DEFAULT_PORT` edit that collides with either — the supervisor
  entry was previously missing from every sibling's guard table, which is
  how it silently collided with `trusty-code`'s old default.
- **trusty-agents proxy route (#3331):** `agents` is now in the reverse-proxy
  allowlist, so the trusty-agents API surface is reachable via `/api/agents/*`.
  Under the loopback-only doctrine (#3328) the agents daemon binds `127.0.0.1`
  by default, making this console proxy the intended remote path to it. A new
  `AgentsConnector` resolves the daemon's live base URL from the standard
  `http_addr` discovery file — the same mechanism the other proxied siblings use
  (`resolve_data_dir("trusty-agents")/http_addr`, gated on the `tagent` binary).
  `all_connectors()` now returns six connectors.

### Changed

- **UI: migrated to Foundry v2 design tokens (closes #3489, refs #3486).**
  `ui/src/theme.css` (and every component that referenced it) dropped the
  independent violet/purple `--color-*` palette — treated as never-migrated
  legacy, not an intentional identity — for the canonical "rust-on-paper"
  Foundry v2 tokens (`docs/design/UI/design-system/tokens.css`), renamed onto
  the shared `--trusty-*` convention used by the other migrated crates. The
  light/dark activation mechanism (`data-theme` on `<html>`, driven by
  `theme.svelte.js`) is unchanged. Two console-specific status tokens with no
  canonical equivalent (`--trusty-status-degraded`, `--trusty-status-absent`)
  were added to preserve the 5-state service-health badge model.
- **Security (internal):** the write-origin (CSRF) guard implementation moved
  to `trusty-common` (`server::origin_guard`); `routes::origin_guard` is now a
  thin re-export so there is exactly one guard implementation shared with the
  sibling daemons. No behavioural change — the existing guard regression suite
  passes unchanged (architecture review tranche 1,
  [#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)).

### Fixed

- **Security (P1):** the write-origin (CSRF) guard is now applied
  router-wide via `Router::layer` instead of a route-scoped `route_layer`, so
  it also covers the reverse-proxied upstream daemon routes
  (`/api/{service}/{*path}`, `/proxy/{daemon}/{*path}`) — previously a
  cross-origin page could reach destructive daemon endpoints (index deletion,
  daemon shutdown) through the proxy unguarded (closes #3268).
- the same guard is now bind-aware: in Tailscale bind mode the console's own
  resolved non-loopback bind address is trusted as an additional self-origin
  (narrowly, not the whole CGNAT range), fixing 403s on the console's own
  write UI when bound on a Tailscale address (closes #3269).
- the cross-crate `default_port_does_not_collide_with_known_siblings`
  port-contract table now also tracks trusty-embedderd's `--http` mode
  default (7890) and trusty-review's corrected default (7891), closing the
  gap that let trusty-review's 7890 collide with trusty-embedderd silently
  (closes #2573).

## [0.4.0] — 2026-07-09

### Changed

- Version reconcile to match already-published crates.io state; no functional change.

## [0.3.0] — 2026-06-16

### Changed (closes part of #1318)

- **Sole binary owner.** The standalone `trusty-console` crate is now the ONLY
  producer of the `trusty-console` binary. The bundled `[[bin]]` shims were
  removed from all five host crates (`trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-mpm`, `trusty-review`) to fix the cargo
  `.crates2.json` binary-ownership collisions that forced `--force` on
  `cargo install` / self-`upgrade` (#1262). Install with
  `cargo install trusty-console`.
- **`run()` decoupled from global argv.** Added `run_from(argv: Vec<String>)`
  as the canonical library entry point; `run()` is now a thin wrapper that
  forwards `std::env::args().collect()`. This lets callers (and tests) drive
  the console deterministically without mutating `std::env`.

### Added (closes part of #1318)

- **`trusty-console port [--json]` verb.** Reports the console's bound (live,
  from the discovery file) or default (`7788`) HTTP port. `--json` emits the
  `{"addr":"<host>","port":<u16>}` envelope consumed by `tctl` console
  discovery (`trusty-controller`), fixing the latent bug where `tctl` spawned
  a `port --json` verb that did not exist.
