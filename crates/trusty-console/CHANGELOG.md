# Changelog — trusty-console

All notable changes to trusty-console are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.11.2] — 2026-09-13

### Added

- The trusty-analyze dashboard is served from the console at `/tools/analyze/`.
  Its Svelte source moved into this crate as `ui-analyze/`, alongside
  `ui-search/` and `ui-memory/`, and build.rs builds it into the committed
  `ui-analyze-dist/` bundle
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- `ANY /api/analyze/{*path}` (and the deprecated `/proxy/analyze/{*path}` alias)
  translate each path the dashboard calls into one `analyze.*` JSON-RPC call on
  trusty-analyze's Unix socket. trusty-analyze has had no HTTP surface since
  #6287, so this is the only way in
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The analyze bridge gives one exchange 120 seconds rather than the 30 the
  other two use. `analyze.clusters` pulls a whole index's chunks out of
  trusty-search and runs k-means over them; on the `trusty-tools` index that
  took 38.8 s, so a 30-second budget refused work the daemon was still doing
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The trusty-memory dashboard is served from the console at `/tools/memory/`.
  Its Svelte source moved into this crate as `ui-memory/`, alongside the search
  dashboard's `ui-search/`, and build.rs builds it into the committed
  `ui-memory-dist/` bundle
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- `ANY /api/memory/{*path}` (and the deprecated `/proxy/memory/{*path}` alias)
  translate each path the dashboard calls into one `memory.*` JSON-RPC call on
  trusty-memory's Unix socket, including the `/sse` live feed, which becomes
  `memory.activity_stream` bridged back to Server-Sent Events. trusty-memory has
  had no HTTP surface since #6286, so this is the only way in
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The live-feed bridge writes its response head without waiting for a first
  event. `memory.activity_stream` sends no opener, so peeking that frame under
  the 60-second open budget held the head for up to a minute against a healthy
  daemon and the feed never connected; the peek now has its own two-second
  budget and its expiry commits to `200`
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The search, memory, and analyze service dashboards each show a "Console"
  link-back in their Topbar: a same-origin relative link when served through
  the console's `/tools/<tool>/` mount, else the well-known standalone
  default `http://127.0.0.1:7788/` (owner ruling 2026-08-31, #6439). No
  config knob.
- The three dashboards' sidebar nav glyphs and status badges now draw from
  the shared Foundry icon set (`docs/design/UI/design-system/icons/
  ActionIcon.svelte`, vendored per dashboard) and the shared Foundry `Badge`
  component, instead of three divergent sets of ad hoc unicode characters
  and hand-rolled status spans (#6439).
- Each of the four host cards (CPU, Memory, Disk, Network) draws a bar graph
  along its bottom edge, one bar per 1 s sample, newest at the right, seeded
  from the history snapshot and appended live from the SSE stream. CPU, memory
  and disk bars band at the same 80/95 (disk 85/95) thresholds the cards' own
  pressure badges use; the network graph is `rx + tx` bytes/sec scaled to the
  busiest second in the visible window
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- One `EventSource` client for the whole page, which seeds from the `history`
  snapshot, appends on `sample` and `services`, re-fetches the snapshot on a
  `lagged` event rather than appending across the gap, drops an unparseable
  frame without closing the connection, and reconnects with backoff
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- Each registered service gets a per-second sample — `{ id, status, cpu_pct }` —
  recorded into a bounded 600-point in-memory ring, fanned out on
  `/api/console/machine-status/stream` as a new `services` event, and carried in
  the `history` snapshot under `service_samples`. `cpu_pct` is `null`, never
  `0.0`, whenever no measurement was taken: an idle bar and an unmeasurable one
  must look different on the card
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `GET /api/console/services` entries carry `cpu_pct`, so the list renders a CPU
  figure before the stream connects. It is read from the same rings the graph
  draws, so the number and the newest bar cannot disagree
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- A service's process is identified from the discovery artifact it already
  publishes: the `pid` line in trusty-mpm's `daemon.lock`, and the peer pid of
  the Unix socket for trusty-search and trusty-memory. trusty-agents serves TCP
  loopback, which carries no pid, and trusty-analyze and trusty-review are
  on-demand members with no resident process — those three report `null`. A
  lookup that fails backs off for 15 s rather than retrying every tick
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- Each Services row on the console home page now shows the service's resident
  memory beside its %CPU, and draws two graphs side by side — CPU first, memory
  second. Both are sampled on the same 1 s tick, stored in the same per-service
  ring and delivered on the same `services` SSE event, so the two bars at any
  point are the same second. The memory graph scales to that row's own peak over
  the window rather than to a fixed ceiling. The screensaver's service table
  gained the same column and graph (#6773).
- `TrustyConsole.saver` bundles a static render of the dashboard's services
  frame. The System Settings gallery tile draws it instead of a text wordmark,
  and the pre-load/offline fallback draws it dimmed under a
  `TRUSTY CONSOLE · OFFLINE` banner. `scripts/render-console-saver-preview.sh`
  regenerates the PNG from the live `/ui/screensaver` page with the Chromium
  `website/`'s Playwright install already caches, so the asset can be refreshed
  whenever the dashboard changes
  ([#6839](https://github.com/bobmatnyc/trusty-tools/issues/6839)).
- A console-hosted event-bus core (DOC-73 §4.1-§4.3): a bounded in-memory ring
  (default capacity 8192, oldest-evicted), a UDS ingest listener at
  `daemon_socket_path("trusty-console")` accepting newline-delimited
  `HarnessEvent` JSON, dedup by event id, and a `tokio::sync::broadcast`
  subscriber seam for the SSE fan-out a later slice adds. A malformed or
  oversized line drops only its own connection; the listener keeps serving
  every other producer. The durable day-rotated NDJSON log and the
  console-assigned `seq` from the wider #6848 scope are deferred to a
  follow-up PR.
- A durable, day-rotated NDJSON event log for the console event-bus (DOC-73
  §4.3, issue #6848 slice 3b), in `crates/trusty-console/src/event_bus/log/`:
  console now assigns the single `seq` on every accepted frame (overwriting
  whatever the producer stamped), recovers that counter's high-water mark from
  the log's tail on restart (a truncated final line is skipped, not fatal),
  and retains a configurable number of days of history with rotation that
  keeps `seq` continuous across the boundary. All log I/O runs on a dedicated
  writer task behind a bounded channel, so a slow disk never blocks ingest — a
  full channel drops the write and counts it (`EventBusMetrics::log_dropped`)
  rather than blocking or failing. A reconnecting subscriber can replay
  everything persisted after a given `seq`; a request that predates retention,
  or a range a dropped write left missing, comes back as an explicit gap
  marker rather than silence. Live-delivered events on the broadcast channel
  now carry a `persisted` marker, `false` for everything fanned out on this
  path — durability is confirmed only for events read back from the log.
- The Services list's `trusty-console` row now opens a details pane for the
  console itself: version, uptime, CPU and resident memory with the same
  side-by-side graphs the roster rows draw, and the count of browser streams
  attached to the machine-status SSE endpoint. Bus status, service connections
  and message rates are stated as not yet available — they wait on the
  event-bus transport (#6460).
- `GET /health` now reports `uptime_secs`, the whole seconds this console has
  been serving. Additive: a caller reading only `status` and `version` is
  unaffected.
- The SPA expected machine-status history schema 3 and logged a mismatch
  warning on every page load against a daemon serving schema 4 (#6915). It now
  expects 4, and a test asserts the constant against the Rust one.
- The console lists itself in the Services roster. `GET /api/console/services`
  now carries a `trusty-console` entry — status `running`, lifecycle `daemon`,
  version read from the binary — through the same generic row path as the other
  six members, with %CPU and RSS sampled from the console's own pid once a
  second like every resident daemon.
- The machine-status history snapshot carries `sse_client_count`: how many
  browser streams are attached to `/api/console/machine-status/stream` at the
  instant the snapshot was built, read live off the broadcast rather than kept
  as a second tally. `schema_version` is `4`. This counts browser streams only;
  it is not a DOC-73 bus subscriber count, and bus status, service connections
  and message rates remain unimplemented pending the bus ingest (#6460, #6862).
- The Trusty Agents row on the Overview opens that daemon's own dashboard in a
  new tab, at the URL console detection already read from its `http_addr` file.
  It was the one service in the roster with no in-console tab, so its row had no
  target at all ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
- New Disk view in the console — projects and worktrees as a segmented radial, coloured by staleness tier (#6929, DOC-73 §16.3). The centre is the workspace-root total, ring 1 one neutral arc per project, ring 2 one arc per worktree in the tier colours the console already defines: `--trusty-success` for safe-to-clear, `--trusty-warning` for review, `--trusty-danger` for keep, and the muted text token for a worktree git registers but whose directory is gone. Hand-rolled inline SVG, no charting library, following `BarGraph.svelte`. Below 600px the same rows render as a sorted list with the same tier colours, and the detail panel — path, branch, tier, the reclaim gate and its reason verbatim, PR, claiming session — renders from the payload already fetched, never a second request. Display-only: nothing here clears a worktree. An arc too thin to hover folds into an "other" wedge that names how many rows and how many bytes it holds and lists every one, so a worktree with no measured byte figure is folded rather than dropped.
- New `GET /api/console/disk/tree` and `GET /api/console/disk/worktrees/{id}`, both proxying trusty-mpm's read-only `disk_survey` MCP tool over the existing stdio bridge (DOC-73 §16.5). Both always send a classification budget — 20 seconds by default, any caller override clamped to 25 — because the console's MCP transport cuts a call off at 30 seconds and an unbudgeted survey outran that on a 48-worktree fleet. Worktrees the budget was reached before are still listed, as `review` with the reason the tool gives, never as safe-to-clear. `{id}` is the worktree's absolute path, percent-encoded; an id the survey does not carry is a 404 naming it.
- The console surfaces the operator's `disk.keep_list` state rather than swallowing it: a config that will not parse holds every worktree, so it renders as a banner saying no row below is a reclaim judgement, and a pattern that would not compile renders as a warning row.
- The search dashboard carries a stale-registration panel at `#/indexes/cleanup`,
  reached from a Stale registrations button on the Indexes screen. It reads
  trusty-search's own `GET /registry/orphans` — which walks `indexes.toml`, so it
  is the only screen that can list a registration the warm-boot allowlist
  excluded — and deletes a confirmed batch through `DELETE /indexes/{id}`, one
  request per id pinned to the root the census reported. Roots the daemon
  declined to judge are listed, never selectable, and settled one at a time
  behind their own confirmation. Two rules are enforced in `lib/cleanup.js` and
  tested there: eligibility reads the daemon's root classification and
  `chunk_count` and never `size_bytes` / `disk_bytes`, which report `0` for a
  healthy 71,433-chunk colocated index (#4706); and a delete counts as a removal
  only when the response BODY carries `ok` and `removed`, so an id the daemon did
  not remove — a `404 removed:false`, or a `500` whose durable cleanup failed
  (#6363) — is shown as not removed rather than as success
  ([#6941](https://github.com/bobmatnyc/trusty-tools/issues/6941)).
- The search, memory and analyze dashboards open their Topbar with the Foundry
  brand lockup — the canonical robot mark beside the tool's own name — and each
  ships a branded `<title>` and the shared trusty favicon. A dashboard served at
  `/tools/search/` previously showed a bare breadcrumb, the generic page icon,
  and nothing identifying the product family
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)). The lockup is
  a new canonical design-system component, `ToolLockup.svelte`
  (`docs/design/UI/design-system/icons/`), vendored into each dashboard with
  `RobotIcon.svelte`; it reads colour and type from Foundry tokens only, so it
  inverts with `data-theme` and introduces no new hue.
- One canonical trusty favicon,
  `docs/design/UI/design-system/icons/favicon.svg`, derived from the Foundry
  robot mark and carried byte-for-byte by every trusty-\* web page: this crate's
  console UI and three dashboards, the public website, and the trusty-audit,
  trusty-code-gui and trusty-mpm-gui shells. Favicons were per-crate and
  divergent, and four of those pages shipped none at all
  ([#7590](https://github.com/bobmatnyc/trusty-tools/issues/7590)). The console's
  own `docs/design/UI/icons/trusty-console-favicon.svg` is deleted with it;
  `trusty-agents` keeps its separate product favicon under the standing owner
  exemption.

### Fixed

- The memory dashboard's activity feed no longer starves the page's main thread. It wrote reactive state once per SSE frame and handed a 500-row keyed list to Svelte each time, so a busy daemon produced a single 46 s task, left 97% of a 69 s window blocked, and made `/health` and `/api/v1/palaces` hit the client's 35 s abort against a daemon answering in 19 ms. Frames are batched into one write per 250 ms window with a bounded number of rows per flush, the stream drops while the tab is hidden, `hook_fired` rows render as text instead of a `JSON.stringify` per row, history paging is capped, and both the Health view and the topbar badge wait for two consecutive failed polls before reporting the daemon unreachable — a cold start below that threshold reads `connecting…`, not `offline`. At 200 events/s for 60 s the worst task is now 96 ms, with none over 200 ms.
- The analyze dashboard's Dashboard, Complexity, Smells, Refactors, Clusters and
  Facts views render their rows. Five `analyze.*` methods answer an object
  wrapping their list — `complexity_hotspots` sends `{index_id, top_n,
  hotspots}`, `smells` a pagination envelope around `chunks`,
  `refactor_suggestions` `{index_id, count, min_severity, suggestions}`,
  `clusters` a `ClusterResponse`, and `facts_list` `{facts, count}` — while
  every view iterates a flat array, so the landing view threw `hotspots.slice is
  not a function` and the other four `not iterable`. `api.js` unwraps each list,
  passing a bare array through unchanged, the same shape of fix
  [#7083](https://github.com/bobmatnyc/trusty-tools/issues/7083) made for the
  memory UI's palace roster. The envelope predates the console bridge: the
  retired HTTP router served these same handler functions, so these views have
  never rendered
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The Smells view groups by the smell the detector actually found. Its rows
  carry `CodeSmell`, which serde renders externally tagged
  (`{"LongFunction": {"lines": 80}}`, or a bare string for a variant with no
  fields), and the view read `category`/`name` off each one, so every row landed
  in a single `unknown` bucket
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- **`/tools/memory`'s Palaces view no longer sits on "Loading palaces…", and the KG palace selector no longer stays empty.** `GET /api/memory/api/v1/palaces` became `memory.palaces_list`, which opens every palace to count it — 8.5-11 s on a 93-palace install, and often past the bridge's 30 s `CALL_TIMEOUT`, so the roster arrived as a `502` (twelve of them in one session on 2026-09-06). Both views ask for `?counts=false` now, which the daemon answers from its registry without opening anything; the per-palace counts were already lazy, since expanding a row has fetched them from `memory.palace_get` since #4682. The activity feed's id→name table asks for the same fast form ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155))
  - A bare `GET /api/v1/palaces` still sends `{}` and still counts, so nothing outside this SPA changes; `the_palace_roster_can_ask_for_names_without_counts` pins both forms end to end through the real router
- **A failed palace roster renders as a failure.** Palaces showed the thrown message in an unlabelled card and then "No palaces yet." underneath — an empty list after a failure read as an empty estate. It names the HTTP status, offers a Retry, and says the roster could not be read; the KG view's selector distinguishes loading, unavailable and genuinely empty, and carries its own Retry ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155))
  - `api.js` throws an `ApiError` carrying `status`, and every request now has a 35 s client budget, so a request that never answers becomes an error a view can render rather than a spinner with no end
- **A failing `/api/v1/status` no longer reads as an unreachable daemon.** `refreshStatus` wrote into the same error slot `getError()` exposes as reachability, so one slow aggregate call spoke for a daemon that was answering `/health` in 25 ms. `/health` is the only call that may set the health snapshot or that error; the status failure has its own `getStatusError()` ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155))
- The memory dashboard's Palaces tree and KG palace picker render again.
  `GET /api/v1/palaces` bridges onto `memory.palaces_list`, which answers
  `{"palaces": [{id, palace, error}]}` where the retired REST route answered a
  bare array — so both views threw `TypeError: S is not iterable` and sat on
  "Loading palaces…". The SPA's api layer now unwraps that wrapper into the
  flat array the views iterate, keeping a palace whose counts could not be read
  visible with its error and unknown ("—") counts
- The history broadcast buffer is sized for the 1 Hz cadence. `EVENT_BUFFER` was
  128, chosen when a tick emitted one event every 5 s; a tick now emits two
  events every second, so a stalled browser was told it lagged after 64 s
  instead of 640 s. It is `HOST_HISTORY_CAPACITY * 2` — 1200 — which is two
  events per tick across the whole 10-minute window, and moves with the cadence
  rather than being a second number that can drift from it
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- A panicking sampler tick no longer stops the history. The loop is one bare
  `tokio::spawn`, so a panic anywhere in a tick ended the task: the window
  froze, every open SSE stream stayed connected emitting nothing, and no log
  line said why. Each half of a tick now runs under `catch_unwind`, logs the
  panic payload at `error!`, and lets the next tick run — a panic in the service
  half leaves a gap in that series while the host graph keeps drawing. The
  loop's `JoinHandle` is kept and logs at `error!` if the task ever ends
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- Test-only: `TRUSTY_DATA_DIR_OVERRIDE` is now guarded by one crate-wide lock instead of two, so a `remove_var` in the `lib.rs` port tests can no longer land inside the connector tests' critical section and send `detect()` at the live daemon on 7880 (#6661).
- Test-only: the closed-port probe assertion targets a privileged, non-ephemeral loopback port rather than a just-freed ephemeral one a parallel test can be handed (#6661).
- The screensaver picks its frame from the wall clock rather than from time
  since the page mounted, and its rotation timer waits out the remainder of the
  current frame before its first transition. System Settings' screensaver
  Preview rebuilds the WKWebView every few seconds, so the old mount-relative
  rotation restarted at frame 0 on every rebuild and the per-service CPU and
  memory frame — 20 s in — never appeared there at all
  ([#6828](https://github.com/bobmatnyc/trusty-tools/issues/6828)).
- `TrustyConsole.saver` no longer shows a black screen while the console is
  unreachable. A load now carries a 5 s timeout and a watchdog, so a daemon that
  has bound its port mid-restart but cannot yet answer drops to the fallback in
  seconds instead of holding the view dark for `URLRequest`'s 60 s default; the
  view overrides `animateOneFrame()` so the fallback is repainted every second
  while the live page is off screen, rather than depending on a navigation
  callback that may never come; and retries run every 5 s for the first three
  minutes of an outage and every 30 s after, picking up the live page as soon as
  one succeeds without restarting the saver
  ([#6838](https://github.com/bobmatnyc/trusty-tools/issues/6838)).
- The System Settings screen-saver gallery showed the generic placeholder tile
  instead of the console preview. The gallery reads
  `Contents/Resources/thumbnail.png` and `thumbnail@2x.png` by name and never
  instantiates the saver view, so the `isPreview: true` draw path added earlier
  could only ever reach the in-pane Preview.
  `scripts/build-console-saver.sh` now derives both files from
  `ConsolePreview.png` with `sips` — 90×58 and 180×116, centre-cropped to the
  tile aspect, matching the pair Apple's `Random.saver` ships
  ([#6839](https://github.com/bobmatnyc/trusty-tools/issues/6839)).
- The screen saver's web view is sized from the view's `bounds` on every
  host-driven resize and again on the way into `startAnimation()`, so a host that
  constructs the view at a preview size or 0x0 and supplies the real screen
  afterwards cannot leave the dashboard mis-sized. `autoresizingMask` stays as a
  second line of defence. The saver's `init` line now records the frame it was
  handed and `startAnimation` records the bounds it owned, both at a log level
  `log show` persists, so a fits-the-screen report carries the geometry rather
  than needing a live `log stream`. `PaintHarness.swift` gained a `--frame WxH`
  argument (default 1280x800, unchanged) and a `resize` mode that grows the view
  from a small start frame and asserts the web view and the page's own viewport
  both match the new bounds.
- `search_uds::routes`' slow-open test builds its oversized request frame from `trusty_common::uds::SOCKET_BUFFER_BYTES` instead of a 512 KiB literal. `uds` now sizes both socket buffers to 1 MiB, so the old figure fit entirely in the kernel, the client's `write_all` returned without parking, and the test stopped proving that the open and the first-frame read share one deadline ([#6896](https://github.com/bobmatnyc/trusty-tools/issues/6896))
- `TrustyConsole.saver` now honours `loginwindow`'s stop request immediately, so
  Touch ID is offered at unlock instead of waiting on a saver that has not
  stopped. `stopAnimation()` used to set the same `.offline` state the retry
  timer reads as "keep trying", leave the navigation delegate attached, and
  navigate to `about:blank` over an in-flight console load — WebKit reported that
  cancellation, the offline handler re-armed the retry timer the stop had just
  invalidated, and the saver went on loading the console for four minutes after
  being told to stop. The stop now enters a terminal state that only
  `startAnimation()` leaves, cancels the in-flight load, and detaches the
  delegate before navigating away, so no late callback can restart the loop
  ([#6900](https://github.com/bobmatnyc/trusty-tools/issues/6900)).
- A failed disk survey now says what failed (#6929). `GET /api/console/disk/tree` answered a transport failure with a bare 502 and `content-length: 0`, and an unreachable daemon with a bare 503, so the Disk view could print nothing but `HTTP 502` at the operator — a number that names no cause and suggests no action. Both arms now carry `{status, hint}`: `survey_failed` names the survey and the console's thirty-second MCP call timeout, `unreachable` names the missing bridge. The daemon's own error text stays in the log, since it can carry a path.
- The Disk view renders those hints instead of the status number, and shows a banner when the survey reports itself `partial` — the budget ran out before every worktree was inspected, so the `review` rows below were listed rather than classified and a missing size is unmeasured rather than zero.
- The reverse proxy's SSRF guard parses the upstream URL's host instead of
  prefix-matching the string (#6945). `is_local_upstream` tested
  `http://127.`, `http://[::1]`, and `http://localhost`, so
  `http://127.0.0.1.evil.com`, `http://localhost.evil.com`, and
  `http://[::1].evil.com` all read as loopback and the proxy would dial them.
  It now requires the `http://` scheme and hands the host to the shared
  `origin_is_loopback` classifier, which accepts only `localhost` or a host
  that parses as a loopback `IpAddr` (`127.0.0.0/8`, `::1`), and which refuses
  userinfo (`http://127.0.0.1:80@evil.com`) outright. Same class as #3319 on
  the inbound CSRF guard, on the outbound sibling that never got the fix.
- The macOS screen saver no longer goes black after its second rotation frame.
  Two behaviours combined. `WallpaperAgent`, which hosts the saver on macOS 26,
  called `startAnimation()` on the already-running view every 20 s to 3.5 min
  with no `stopAnimation()` between, and every one of those calls reset the state
  and reloaded, so the dashboard restarted from its first rotation frame and the
  hourly reload timer was re-armed often enough never to fire. Separately,
  RunningBoard moved the WebContent process to `running-suspended-NotVisible` and
  WebKit ran `freezeAllLayerTrees` → `destroyRenderingResources` →
  `markAllLayersVolatile`, discarding the page's backing store without
  terminating the process — so `webViewWebContentProcessDidTerminate`, the view's
  only exit from the live state, never fired and `draw(_:)` went on deferring to
  a compositor with nothing left to composite. A re-entrant `startAnimation()`
  over a live page now reloads nothing, and while live the view asks the page
  every 10 s what `document.visibilityState` says; anything but `visible`, or no
  answer inside 3 s, puts the dimmed preview up and reloads. It reloads on any of
  four grounds — the saver's own window is not occluded, a visible-then-hidden
  transition was seen, the page stopped answering, or the page has been unhealthy
  for 60 s — so no page can sit suspended behind a live-looking state until the
  hourly reload, and no two recoveries run inside 60 s, so none of the four can
  become a reload treadmill. Every decision names its trigger and its ground in
  the `com.trusty.console.saver` log. `PaintHarness.swift` gains `suspend` and
  `suspend-cold` modes: the first also asserts a page reporting itself visible is
  left alone for 35 s, the second that a page hidden from its first answer still
  recovers.
- The memory dashboard's knowledge-graph view no longer freezes the tab on "load everything". It ran the force simulation on `setInterval(tick, 16)` and reassigned the reactive `nodes` array at the end of every tick, so all 200 steps of a re-layout each repainted the whole SVG — on the live trusty-tools palace that is 1,242 nodes and 5,000 edges, 7,484 SVG elements, repainted 200 times, and a tab that stayed unresponsive for over twelve minutes. The steps were never the cost: one is 3.15 ms, 0.63 s for all 200; one repaint of that DOM measures 180 ms, so the paints alone were 36 s before Svelte's own reactivity. The simulation now runs in a Web Worker and commits at most one position snapshot per animation frame, coordinates moved out of the reactive node array into a plain `Float64Array` so a frame no longer re-derives the edge and degree maps, and above 400 edges the edges collapse from one `<line>` each into a single `<path>` — 43 ms per commit over ~20 commits instead of 180 ms over 200. That merged path carries no per-edge marker, so a view rendering more than 400 edges draws them without the directional arrowheads the per-line form shows; below the threshold, and in the default seed view at 328 edges, the arrows are unchanged. The view shows a progress bar with a working cancel button while the layout runs, and says so when it clips the settling passes on an oversized graph. Full load also rendered nothing at all once it stopped freezing: it derives its node list from triple endpoints, so every id arrived once per incident edge, and the membership check read a snapshot taken before the merge loop rather than the set the loop was filling, which let the repeats through and made the keyed `{#each}` throw `each_key_duplicate`. First paint of the full graph is now 440 ms.
- `scripts/install-console-saver.sh` restarts System Settings, `legacyScreenSaver`
  and `WallpaperAgent` after a successful install. All three cache the saver
  bundle's display-name metadata, so after #7129 renamed the bundle's
  `CFBundleName`/`CFBundleDisplayName` to "Trusty Console" the Screen Saver tile
  kept showing the old name across a reinstall until those processes were
  killed by hand. The script now does that itself and prints which processes it
  restarted ([#7128](https://github.com/bobmatnyc/trusty-tools/issues/7128)).
- The console event-bus ingest listener (#6848) no longer buffers an unbounded amount of memory for a peer that streams a line with no newline — the per-line read cap is now applied before each read (a fresh budget re-taken per line) rather than checked only after an unbounded read returns. It also binds through `bind_singleton_hardened`, so a stale socket file left by an unclean shutdown is reclaimed instead of wedging every future bind, adds a 60 s per-connection idle timeout, and bounds concurrent connections at 256 via a semaphore.
- The ingest listener's shutdown signal is now observed even while the connection pool is fully saturated — the permit acquire used to sit outside the accept/shutdown race, so with every permit held the loop could not see a shutdown until a slot freed on its own.
- The analyze dashboard renders in the faces its tokens ask for. `index.html`
  loaded Inter and JetBrains Mono while `styles/tokens.css` has always resolved
  `--trusty-font` / `--trusty-mono` to IBM Plex Sans / IBM Plex Mono, so neither
  requested face was ever used and the page fell back to the system stack. It
  now loads the Foundry three-face set that ui-search and ui-memory already
  load, which is also where the brand lockup's Chakra Petch wordmark comes from
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)).
- The product name in each dashboard's sidebar is readable in both themes.
  `.brand-title` in ui-search and ui-memory used `--trusty-text-inverse`, the
  contrast colour for a rust fill, which flips to `#201612` in the dark palette
  — near-black on the `#171009` sidebar, about 1.1:1, so "Trusty Search" and
  "Trusty Memory" were invisible in dark. ui-analyze had the mirror of it,
  taking the page body colour `--text`, which on the light palette is `#2b1c12`
  against the same always-dark sidebar. All three now use
  `--trusty-sidebar-text` (`#e6d8c8` in both palettes), the token every other
  label on that surface already uses
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)).
- The screen saver rebuilds its `WKWebView` after three consecutive failed
  loads, instead of reloading forever into a WebContent process the OS has
  frozen. A reload lands in the same process, so a page macOS has marked
  NotVisible comes back hidden and the recovery reloads it again: the owner's
  saver ran that loop for three days, showing the bundled offline preview while
  `/ui/screensaver` answered in under a millisecond throughout. The rebuild is
  decided on the failure COUNT alone, never on what the page reports about its
  own visibility — a page reporting itself hidden inside a saver the host is
  animating is not evidence of occlusion. It logs the count and the reason to
  `com.trusty.console.saver` and is rate-limited to one a minute, widening to
  one per ten minutes once the console has been down longer than the fast-retry
  window, so a genuinely dead daemon does not turn into process churn
  ([#7606](https://github.com/bobmatnyc/trusty-tools/issues/7606)).
- A retry no longer cancels the load it is retrying. The retry delay was a flat
  5 s against a 6 s load deadline, so every retry issued a `load` over an
  attempt WebKit had not finished with; WebKit reported that supersession as
  `NSURLErrorCancelled` (-999), the view read its own cancellation as a fresh
  network failure and armed another 5 s retry, and the loop fed itself — 139 of
  those in two hours of the owner's log. The delay is now derived from the
  deadline rather than chosen, and the view cancels any attempt it abandons and
  recognises the cancellation that comes back, so a -999 it counts can only have
  come from outside ([#7606](https://github.com/bobmatnyc/trusty-tools/issues/7606)).

### Changed

- The analyze dashboard no longer opens an `EventSource` on `/sse`, and its
  `sse` status pill is gone. #6287 deleted that route and the `AnalyzerEvent`
  broadcast behind it without putting a streaming RPC method in their place, so
  the subscription reconnected forever against nothing. The ten-second
  `/health` poll is the liveness signal that remains
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The Server-Sent Events plumbing both UDS bridges use — the `data:` encoding,
  the 20-second keep-alive, the cancel-safe reader, the terminal error event —
  moved out of `search_uds::routes` into `uds_sse`, so the memory bridge shares
  it rather than carrying a second copy
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The router moved out of `server/mod.rs` into `server/router.rs`; the five new
  memory routes pushed that file past the 500-SLOC cap. `build_router` and its
  two siblings are re-exported, so no call site changed
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- Removed the `trusty-embedderd` 7890 row from the `known_siblings` port-collision guard; that listener is retired under ADR-0032 and the port is free ([#6289](https://github.com/bobmatnyc/trusty-tools/issues/6289))
- The crate-private `uds_sse` module moved to `trusty_common::uds::sse`; the
  search and memory bridges import it from there. No behaviour change — the
  `data:` encoding, the 20-second keep-alive, the cancel-safe reader task and
  the terminal error event are the same bytes on the wire
  ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637)).
- The console home page has one services section, titled "Services": an
  alphabetical list carrying name, version, status, %CPU and a per-second CPU
  bar graph per row. Clicking a row opens that service's dashboard; a service
  with no dashboard renders inert and says so. The "Installed Services" card
  grid and the machine-status rollup table are both gone, along with
  `ServiceCard.svelte` and `cardActions.js`
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `--host-sample-interval` defaults to 1 second instead of 5, and the history
  window holds 600 points instead of 120. The span is the same ten minutes and
  the payload still advertises the cadence in use, so an operator who raises the
  interval stretches the span the same 600 points cover
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `HistorySnapshot.schema_version` is `2`. The payload gained `service_samples`
  and `service_sample_capacity`; a client built against schema 1 parses it fine
  but renders no per-service graph, which is the difference the version
  announces ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- The `/ui/screensaver` route draws the same live 1 s bar graphs the home page
  does: one `machineStream.js` EventSource for the page seeds the window from
  the history snapshot and appends every second, the four host cards carry a
  graph on their bottom edge, and the newest streamed sample sets the card's
  headline number so it and the rightmost bar are the same second. Its service
  frame is now the alphabetical roster from `servicesList.js` — name, version,
  status, %CPU and a per-row CPU graph — rendered as an inert table, so nothing
  on the route is a button and nothing takes focus. The frame-0 service tally
  is that same list counted, so the two frames can no longer report different
  services. Idle entry, the fullscreen gesture and the poll backoff are
  unchanged ([#6643](https://github.com/bobmatnyc/trusty-tools/issues/6643)).
- `machineStatus.js` lost `serviceRows`, `serviceHealthTone` and `rollupTone`
  with the last view that rendered the metrics rollup as a table
  ([#6643](https://github.com/bobmatnyc/trusty-tools/issues/6643)).
- The search console's Indexes roster flags an index whose vector store is
  empty. Its Status column showed `ready` — the reindex-queue state, not a
  health verdict — so an index holding 58,415 chunks and 0 vectors, which
  answers nothing for every vector query, rendered green. The column now shows
  `Degraded` with the fault sentence on hover, computed by the same
  `indexHealth` the expanded per-index panel's banner uses, so the row and the
  panel cannot word the same fault differently
  ([#6699](https://github.com/bobmatnyc/trusty-tools/issues/6699), follows
  [#6689](https://github.com/bobmatnyc/trusty-tools/issues/6689)).
- The roster draws from one `GET /indexes?details=true` call instead of
  `GET /indexes` followed by a `GET /indexes/{id}/status` per row — 42 requests
  down to 1 on a 41-index daemon, with the same per-index directory walk count
  and the same column values
  ([#6699](https://github.com/bobmatnyc/trusty-tools/issues/6699)).
- The console robot mark animates: the three readouts on its panel light in sequence on a 1.8s loop, and the whole unit rocks a few degrees and settles once every 16s. Both are CSS keyframes on `BrandMark.svelte`, so the header lockup, the screensaver at `/ui/screensaver` and the overview loading panel all get them from one definition; both stop under `prefers-reduced-motion: reduce`, and only `transform` and `opacity` animate, so nothing on the page moves or resizes (#6733).
- The console's top tab bar is gone; the Services list is the navigation. Every
  row that opened a tab still opens the same view — search, memory, analyze,
  review and MPM sessions — and the header now carries the two things no
  Services row could: a Config action beside the theme control, and a
  breadcrumb back to the Overview from any detail view. The breadcrumb is a
  button, so it is keyboard reachable, and it follows the Foundry topbar's
  crumb-left, actions-right layout (#6909).
- Each Search-tab index row opens that index's management view in the
  console-served search dashboard, at `/tools/search/#/indexes/<id>/config`.
  That is the route the dashboard actually has — it is hash-routed and serves no
  `/tools/search/indexes/<id>` path — so the roster is now a grid list whose row
  is the link, the way the Services list already works
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
- The Search tab's second stat card is labelled "Indexes Degraded" rather than
  "Warm Boot Degraded", which named the field instead of what it measures: any
  index not fully serving, whether from a failed embed stage, a boot-time TCC or
  allowlist skip, a load timeout, or a registry smaller than it was. Both stat
  cards centre their text
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
- The console's Memory tab is display-only: it shows the palace store's disk usage and the daemon's physical footprint with its heap / file-backed / compressed split, and every palace row links to that palace on the memory dashboard. Compact and delete moved to `/tools/memory`, where the dashboard's hero row also gained aggregate RAM and Disk tiles beside Palaces, Drawers, Vectors, Rooms and KG Triples.
- The screen saver's tile in System Settings > Screen Saver > Other reads
  `Trusty Console` instead of `TrustyConsole`. The bundle's `Info.plist` now
  sets `CFBundleDisplayName` and `CFBundleName` to the spaced form; the bundle
  identifier `com.trusty.console.saver`, the `TrustyConsole.saver` filename, the
  `CFBundleExecutable` and the principal class are unchanged, so launchd, the
  `log` subsystem predicate and Settings' selected-saver state still resolve
  ([#7128](https://github.com/bobmatnyc/trusty-tools/issues/7128)).
- The console's header/nav bar now stays pinned to the top of the viewport;
  Services, Sessions, event lists, and dashboard sections scroll beneath it
  instead of scrolling it off-screen (#7167).

### Removed

- The Search tab's per-row index delete and its stale-index cleanup panel, with
  the nested unjudged-registration review inside it. The console displays and
  the dashboard manages, so deleting an index is reachable only from
  `/tools/search`. The `/api/console/search/prune-indexes` and
  `deregister-unjudged` routes are untouched and keep working; nothing in the
  console calls them until the dashboard carries that panel
  ([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)).
- `POST /api/console/search/prune-indexes` and
  `POST /api/console/search/deregister-unjudged`, with `routes::census_guard`
  and the `ActionVerdict::reason` / `::id` accessors that existed only for the
  batch's per-id rows. #6923 left both routes serving with no caller; the search
  dashboard now carries that panel and calls trusty-search's own
  `GET /registry/orphans` and `DELETE /indexes/{id}` directly, so a console
  management POST would be a second path to the same work — and console is
  display-only (DOC-73 §13). `crates/trusty-console/ui/src/cleanupFlow.js` keeps
  only the palace-compact half the Memory tab still uses
  ([#6941](https://github.com/bobmatnyc/trusty-tools/issues/6941)).

## [0.10.0] — 2026-09-02

### Added

- `GET /api/console/machine-status/history` returns the last 10 minutes of host
  samples (120 points at 5 s) plus the per-service transition log, oldest first.
  Before the first sample it answers 200 with empty arrays rather than the 503
  the point-in-time route returns — an empty window is a complete answer (#6641).
- `GET /api/console/machine-status/stream` is a `text/event-stream`. It opens
  with one `history` event carrying the current window, then sends a `sample`
  event per new sample and a `transition` event per service state change. A
  subscriber that falls behind the broadcast buffer gets a `lagged` event naming
  the dropped count instead of a silent gap (#6641).
- A per-service transition log records only the moments a service's derived state
  (`up` / `degraded` / `down` / `unknown`) changed — never a row per poll. A
  service whose report goes stale past a 60 s grace window, or that drops out of
  the report set for that long, transitions to `down`; a retained cache entry is
  not evidence a service is alive (#6641).
- `serve --host-sample-interval` sets the host sampling cadence, default 5 s. It
  is independent of `--poll-interval` (still 15 s), which drives the stdio-MCP
  service polls. The history payload advertises the configured value so the
  graph's x-axis follows it (#6641).

### Fixed

- Console pages scroll again. `Screensaver.svelte` clipped `:global(body)`, and
  Vite emits one CSS bundle for the whole SPA, so that rule applied on every tab
  whether the screensaver was mounted or not — later than `App.svelte`'s `body`
  rule at equal specificity, so anything taller than the viewport was
  unreachable. The screensaver still clips itself through `.saver`, which is
  `position: fixed; inset: 0; overflow: hidden` (#6658).
- The search dashboard this crate builds and serves at `/tools/search/` no longer shows an index with an empty vector store as green. Its semantic lane badge came straight from `stages.semantic.status`, which reports on the last embedding pass rather than on what the vector store holds; it is now computed from the stage, the advertised `vector` capability and `semantic_coverage.vectors_present` together, and an index with chunks that advertises vector search while holding zero vectors renders `Empty` in red under a `Degraded` verdict ([#6689](https://github.com/bobmatnyc/trusty-tools/issues/6689))

### Changed

- The analyze detector's `analyze.health` frame carries `"params": {}` (#6555). It sent no `params` at all, which decodes to `Value::Null` and works only because `analyze.health` is bound to `NoParams`; binding that method to a struct would have turned the omission into a `-32602` and read a healthy daemon as absent. The memory connector was fixed the same way in #6359 — this is the sibling site that change missed
- The Trusty Analyze card's status now comes from a connect-only socket probe
  rather than an `analyze.health` RPC on every poll. The version is still shown:
  it is read once per daemon lifetime and cached against the socket's inode, so
  a respawned daemon is re-read while an open dashboard no longer dials the
  daemon four times a minute (#6621).
- History is in-memory only and resets on restart, by owner ruling — a restarted
  console begins a new, empty window (#6641).
- `host_status::start` is gone; one loop in `machine_history::sampler` now writes
  both the point-in-time host cache and the history ring, so the two can never
  disagree about the newest sample. `host_status::HostMetricsCache` is unchanged
  (#6641).

### Documentation

- Repair the machine-history sampler link so rustdoc resolves its sample-recording entry point.

## [0.9.2] — 2026-09-01

### Added

- `GET /api/console/machine-status`: aggregated whole-machine status combining
  host resources (CPU, memory, disk, network) with a per-service health rollup.
  A background sampler (`host_status`) keeps the host snapshot warm; the route
  assembles `MachineStatus` from it plus the cached per-service reports. Data
  endpoint for the phase-2 Foundry dashboard (#6517).
- The Overview tab leads with a whole-machine status dashboard built to the
  Foundry design system: a four-card row for host CPU, memory, disk and network,
  each stamped with the pressure band the server classified, and a rollup card
  counting every reporting service with a per-service table of version, health
  and collection time. It polls `GET /api/console/machine-status` every 15s and
  says "first sample pending" while the host cache is still cold, rather than
  reporting HTTP 503. The existing per-service card grid stays below it — that
  grid is the only place a never-installed or absent service is visible (#6518).
- A fullscreen screensaver route at `/ui/screensaver` (also reachable at
  `/screensaver`), rendering the machine-status data across the whole viewport
  with no tabs, no theme selector and no scrollbars. It forces the dark palette,
  shows a live clock beside the brand lockup, and rotates every 20 seconds
  between the four host stat cards with service counts and the full per-service
  table. It renders with no user interaction, which is what the coming macOS
  `.saver` bundle needs (#6519, #6520).
- The screensaver survives an unreachable daemon: a failed poll keeps the last
  good snapshot on screen behind an "updated Xm ago" line instead of an error
  box, and the 15s poll doubles after three consecutive failures up to a 60s
  ceiling, resetting on the first success (#6519).
- Optional idle entry, **off by default**: set the `localStorage` key
  `trusty-console-screensaver-idle-minutes` to a positive number of minutes and
  the console navigates to the screensaver after that long without a mouse or
  key event; any input there returns to `/ui`. There is no settings UI for this
  key yet — set it from the browser console. On the screensaver's own URL the
  first click or keypress requests fullscreen (a no-op where the browser refuses
  it) and the next one leaves (#6519).
- A native macOS screen saver, `TrustyConsole.saver`, that displays the console
  dashboard: one `ScreenSaverView` hosting a `WKWebView` on
  `http://127.0.0.1:7788/ui/screensaver`. When the console is unreachable it
  paints a native Foundry-dark fallback and retries every 15s; the System
  Settings thumbnail renders the wordmark rather than spinning up a web view; and
  it reloads hourly for long-run memory hygiene. Port and route are overridable
  via `defaults -currentHost write com.trusty.console.saver ConsolePort <port>`.
  Source in `crates/trusty-console/macos/saver/`, built and installed by
  `scripts/build-console-saver.sh` and `scripts/install-console-saver.sh` —
  ad-hoc signed by default, Developer ID with `CODESIGN_IDENTITY` set. macOS-only
  (#6520).
- The header lockup names the running console version — `UNIT-05 · SERVICE CONSOLE · v0.9.2`. The version is read from the server's existing `GET /health` on mount rather than compiled into the SPA bundle, which is committed and would otherwise go stale; until that probe answers, the descriptor renders unchanged.
- The search dashboard's index rows expand to a per-collection indexing
  pipeline: a badge per lane (lexical, semantic, graph) with its counters, an
  embedding pause/resume toggle, and a live feed of the last 200 file changes the
  watcher saw. Pausing stops embedding only — lexical search, the knowledge graph
  and the watcher keep running — and the pause is in-memory, so it clears when
  the daemon restarts; the panel says so. An expanded row polls its status every
  15s and holds one SSE feed; collapsing it stops both (#6524).
- Three `/api/search/…` rows onto the daemon's socket methods:
  `POST /indexes/{id}/embedding/pause` and `.../resume` onto
  `search.index.pause_embedding` / `search.index.resume_embedding`, and
  `GET /indexes/{id}/file-events/stream` onto the `search.index.file_events`
  stream (#6524).

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
