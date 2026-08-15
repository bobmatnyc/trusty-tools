# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.23.1] — 2026-08-15

### Breaking

- `triple_id` in `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` is now the base64url encoding of `subject + "\0" + predicate + "\0" + object`. An id in the old two-field form is rejected with `400` and a message naming the new format — it cannot identify a single triple, and accepting it is what closed every object at the pair. Ids are derived from the fields, never persisted or returned by any endpoint, so a caller rebuilds one by adding the object.
- `MemoryService::kg_retract_triple` takes an `object` and returns the number of rows closed (`usize`) instead of `bool`.
- Bare `trusty-memory serve` now speaks MCP over stdio instead of detaching an
  HTTP daemon, matching `trusty-search serve`. Use `trusty-memory start` for the
  background daemon. Every flagged form is unchanged: `serve --stdio`,
  `serve --http <ADDR>`, `serve --foreground`, and `start`. A human running bare
  `serve` at a terminal gets a one-line stderr notice explaining the move; MCP
  clients (piped stdin) see nothing.

### Added

- **`kg_retract_triple` MCP tool — the inverse of `kg_assert`.** A triple
  asserted over MCP could not be taken back over MCP: `remove_prompt_fact` is
  scoped to hot predicates and closes the whole `(subject, predicate)` pair,
  and re-asserting adds an object rather than replacing it for any predicate
  outside the functional set. The tool takes the full
  `(subject, predicate, object)` key, closes exactly that triple, and leaves
  every sibling object at the pair active. It returns
  `{palace, subject, predicate, object, closed, retracted}` — `closed` is 1 for
  a retraction and 0 when nothing matched, so a miss is a legible no-op rather
  than a silent one, and a repeat call is safe. `object` is required: omitting
  it is an error, not a pair-wide retraction. Retracting a hot-predicate triple
  rebuilds the prompt cache so the fact stops being injected.
- **`/health` now reports live worker-pool occupancy (issue #4001).** The
  payload carries a `worker` block — `in_flight`, `oldest_age_secs`, and a
  `wedged` verdict — and the top-level `status` becomes `"wedged"` when the
  oldest in-flight palace operation has outlived
  `TRUSTY_WEDGE_THRESHOLD_SECS` (default: twice `open_queue_timeout()`).
  Backed by a new `worker_liveness` module: a fixed-size, lock-free slot table
  of operation start timestamps, registered from `open_palace_handle` (the
  choke point every `memory_recall` / `memory_remember` passes through, and
  where the #3992 wedge actually occurred). One CAS in and one store out per
  operation, no allocation and no syscall on the hot path, so the gauge cannot
  become the load problem it exists to detect. Registration is RAII, so the
  `?` and panic paths release it as reliably as the success path.
- **Progressive (seed + expand) loading for the palace knowledge-graph view
  (issue #4670).** The view used to fetch the entire graph in one call and lay
  it out with a hand-rolled O(n²) force simulation explicitly budgeted for
  "<500 triples"; the `trusty-tools` palace is now 8,266 triples / 9,311 nodes,
  16.5x that budget. Two new read endpoints back a bounded first paint plus
  click-to-expand:
  - `GET /api/v1/palaces/{id}/kg/graph/seed?limit=N` — the `N` highest-degree
    nodes and the edges among them, computed over the already-resident
    `petgraph` adjacency (no new storage, no new dependency). `limit` defaults
    to 75 and is clamped to `[1, 200]`. Measured 5.9 ms / 7.2 KB against the
    live 8,266-triple palace, versus 11.5 ms / 1.06 MB for the full graph.
  - `GET /api/v1/palaces/{id}/kg/graph/neighbors?node=X&direction=in|out|both&max_hops=N`
    — direction-aware, hop-bounded expansion around one node. `max_hops`
    defaults to 1 and is clamped to `[1, 4]`; an unparseable `direction` is a
    400. Parameter names and clamping mirror `trusty-search`'s
    `graph_neighbors_handler` so the two crates share one traversal vocabulary.
    Measured 0.5–0.8 ms on the live palace. **`direction=in` is the first HTTP
    route that can reach a node's incoming edges at all** — `GET /kg?subject=X`
    is a subject prefix scan and never reads the object side, so on the live
    palace the highest-degree node (48 edges, all inbound) returned *nothing*
    through the old route.
  - The graph view now loads the seed on mount, merges expansion results
    deduplicated by node id and by `(subject, predicate, object)`, and pins
    existing node positions during re-layout so an expansion grows outward
    instead of reshuffling the canvas. Nodes are sized by degree and carry a
    dashed halo when they still have unfetched edges. Full-graph load remains
    available as an explicit, size-warned opt-in.
- **`kg-rebuild --purge-stale-subjects`** deletes auto-extracted subjects the
  new filter would now reject. The forward filter only stops new garbage:
  `rebuild_one` re-asserts and never retracts, and the four pattern predicates
  are not in `FUNCTIONAL_PREDICATES`, so an `assert` supersedes only an
  identical object and every triple already in the graph would otherwise stay
  there permanently. The flag is off by default, prints every subject it
  removes, and takes `--dry-run` to report the list while writing nothing at
  all. Selection is deliberately narrow — a subject is skipped if it sits under
  the `drawer:`/`tag:`/`topic:`/`room:` namespaces, or if any of its active
  triples was not stamped `auto:remember`, so one hand-asserted fact protects
  the whole subject. A subject whose delete fails is reported as `[purge-FAILED]`
  on stderr, kept out of the deleted count, and makes the command exit non-zero —
  a failure is never printed as a deletion. `--dry-run` reaches the graph without
  hydrating any palace, so the preview cannot trigger the issue-#61 expired-drawer
  reclamation sweep that `PalaceHandle::open` performs; it genuinely writes
  nothing.
- **`kg_list_subjects` MCP tool — discover a palace's KG subjects instead of
  guessing at them** (closes [#4776](https://github.com/bobmatnyc/trusty-tools/issues/4776)).
  `kg_query` needs a subject the caller already knows, and a subject that does
  not exist returns the same empty result as an empty graph, so a guess was
  indistinguishable from a miss. The enumeration already existed at the HTTP
  layer (`GET /api/v1/palaces/{id}/kg/subjects`); this exposes it over MCP.
  Returns `{palace, subjects, with_counts, truncated}` — bare subject strings,
  or `{subject, count}` pairs under `with_counts: true` — alphabetically, with
  `limit` defaulting to 50 and clamped to `1..=200`. `truncated` is set when
  the page filled to the effective limit, so a partial view is never mistaken
  for the whole graph. The two page-size bounds moved to
  `service::core_kg` so the tool and the HTTP routes read one definition; they
  previously lived in `web::kg_routes`, which is not compiled without the
  `axum-server` feature.
- `memory_note` accepts an explicit `room` (ADR-0027 T5). It was hard-pinned to
  `General`, so a curated fact could not be filed anywhere else. The argument is
  optional, goes through the same single `RoomType::parse` as `memory_remember`,
  and still defaults to `General` — a caller that passes nothing is unaffected.
- MCP room surface: `room_list`, `room_create`, and `room_rename` (ADR-0027 T6).
  `room_list` is the discovery primitive the product never had — before it, a
  caller could not learn that a palace had a `decisions` room without already
  knowing the word. `room_create` is idempotent and matches names
  case-insensitively. `room_rename` is the repair path for rooms the migration
  could only name `unresolved-<id>`; it changes a room's NAME and nothing else —
  no drawer is moved, reassigned, or rewritten — and refuses a name already
  owned by another room.
  - `palace_info` gains `room_count`.
  - The `wing` argument is declared on `room_list` / `room_create` so its name is
    stable, and is validated strictly: anything other than the palace's default
    wing is an explicit error rather than a silently ignored argument, because
    wings are not implemented until ADR-0027 T9.
- `memory_recall` and `memory_recall_deep` accept a `room` scope (ADR-0027 T7).
  The filter has worked in the retrieval layer since #3274, but the MCP schema
  carried only `palace`/`query`/`top_k`, so room-scoped recall was reachable
  only through `memory_list` or the HTTP `/recall` route. While the embedder is
  still warming, the lexical fallback lane is filtered too, so a room-scoped
  recall never returns another room's drawer because of daemon state.
- `trusty-memory rooms backfill --dry-run | --apply` (ADR-0027 T10) — the
  operator audit path for a migration that runs against live palaces. `--dry-run`
  prints the label each room would be given, by which confidence step, and how
  many drawers sit behind it, then exits WITHOUT writing; `--apply` is required
  to write. It opens palaces directly rather than through the registry,
  deliberately: every registry open path runs the backfill itself, so going
  through it would have written the very rows the dry run exists to preview.
- Wing MCP surface — `wing_list`, `wing_create`, `wing_rename` (ADR-0027 D2, ticket T9, closes [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - `wing_list(palace)` is the discovery primitive: every wing with its label,
    description, room count, and whether it is the default
  - `wing_create(palace, label)` is idempotent — case-insensitive matching, first-seen
    spelling kept for display, and creating `default` returns the palace's existing
    default wing rather than a second one
  - `wing_rename(palace, wing, new_label)` retires the old label (a rename, not an
    alias) and provably touches no room and no drawer, since rooms reference a wing
    by id
  - `memory_remember` gains an optional `wing`, so a write can place its room in a
    named scope; `memory_recall` and `memory_list` gain an optional `wing` that
    restricts results to the rooms that wing owns
  - omitting `wing` everywhere is the pre-wing behaviour exactly — the argument is
    never required, and wing-less writes land in the palace's default wing
  - an unknown `wing` is a loud error naming `wing_list`, never a silent empty
    result; `memory_list` likewise rejects `wing` and `room` together rather than
    honouring one and dropping the other
  - `memory_recall_deep` takes `wing` too, so no recall path silently ignores a scope
  - the `room`/`wing` scope is resolved BEFORE the embedder-warming short-circuit, so a
    scoped recall issued while the daemon is warming is filtered rather than returned
    unscoped
- `memory_remember` and `memory_note` take ADR-0028 Tier C arguments: `fact_key` (a namespaced slot, `pr:4818/state`) and `expires_at` (RFC 3339). Writing an occupied slot retires its prior occupant, which is what stops a point-in-time fact being asserted for weeks after it stopped being true — the estate's most-injected drawer today is a 19-day-old session checkpoint reaching 44.8% of turns. The response envelope now reports `tier` (`"C"` or `"E"`) and, when a slot was requested and refused, `tier_c_refused` naming the reason, so a writer whose slot never took effect is told rather than left guessing. A malformed `expires_at` string is an error (a caller typo, not a degradation); a malformed key or an already-elapsed timestamp degrades to an ordinary drawer (closes [#4886](https://github.com/bobmatnyc/trusty-tools/issues/4886))
- Tier S write-time admission control — a hard cap of 20 standing facts and an 80-character form constraint (ADR-0028 D2/D8, closes [#4888](https://github.com/bobmatnyc/trusty-tools/issues/4888))
  - the four hot predicates (`is_alias_for`, `has_convention`, `is_fact`,
    `is_shorthand_for`) feed the always-injected prompt surface, which is paid for on
    every turn of every session; the 21st fact is now REJECTED at write time rather
    than silently dropped or truncated at read time
  - the rejection is actionable: it names all 20 facts currently occupying the surface
    with their `subject`/`predicate`, and names `remove_prompt_fact` as the tool that
    retires one, so the caller can choose what to trade away
  - an object longer than 80 characters is rejected with its actual length and the
    limit; a rule that does not fit is a document and belongs in `CLAUDE.md` with a
    pointer
  - both rejections are fail-closed — the write does not reach storage
  - enforced on every path that can create a hot triple, not just the two obvious
    ones: the `kg_assert` and `add_alias` MCP tools, the `discover_aliases`
    auto-discovery loop, the **chat assistant's `kg_assert` tool** (`POST /api/v1/chat`),
    `POST /api/v1/palaces/{id}/kg`, `POST /api/v1/kg/aliases`, and the offline
    `kuzu-migrate` relation import. Two of these were live bypasses rather than
    hypothetical ones: the chat tool takes `predicate`/`object` straight from the
    model on a surface users hit every turn, and the HTTP KG endpoint is where
    `trusty-mpm`'s provisioner seeds its identity fact
  - the cap cannot be raced past: counting active facts and then writing is two
    steps, and nothing else serialized them — the KG's single-writer actor orders
    writes only within one palace while the count spans all of them. A new
    admission mutex is held from the count through the write. Measured before the
    fix: 16 concurrent writers contending for 1 free slot were all admitted,
    landing the surface at 35
  - the offline `kuzu-migrate` import refuses hot predicates outright rather than
    counting free slots. A bulk legacy import is not a deliberate act of authoring
    a standing rule, which ADR-0028 D8 point 3 requires, so legacy relation data
    never reaches Tier S no matter how much room is left. Refusals join the
    existing warn-and-skip path and name `kg_assert` as the way to author the fact
    deliberately, where the real gate applies. This removes the cap arithmetic
    from that path entirely, and with it any way for the offline gate to be off by
    one or to fail open
  - `discover_aliases` is the one bulk writer, and it stops at the cap instead of
    aborting: aliases that fit are written, the rest come back in a new `rejected`
    array with a single `rejected_reason`, alongside a `complete` flag so a caller
    reading only `new`/`already_known` cannot mistake a partial batch for a whole
    one. A workspace with more crates than Tier S has slots is ordinary — this one
    has — so aborting would both make the tool unusable there and strand the
    aliases written before the refusal
  - the cap counts ACTIVE facts only: retracting a fact frees its slot immediately,
    since retraction closes the interval rather than deleting the row
  - re-asserting an already-active `(subject, predicate)` in the same palace is a
    replacement, not an addition, and stays admitted at the cap — otherwise an author
    who filled the surface could never correct an existing rule
  - cold (non-hot) predicates are untouched: neither limit applies to ordinary
    knowledge-graph writes, which never reach the injected surface
- **Quarterly re-affirmation for Tier S standing rules, and a `trusty-memory
  doctor` check that reports overdue ones (issue #4890, ADR-0028 D8 point 4).**
  The write-time cap of 20 landed in #4895 and stops the always-injected surface
  from *growing*; it does nothing about a rule that was true when written and
  quietly stopped being true. Such a rule keeps its slot forever and is
  re-transmitted on every turn of every agent session.
  - Every Tier S fact now carries `affirmed_at`, surfaced on both read paths:
    the `list_prompt_facts` MCP tool and `GET /api/v1/kg/prompt-facts`. The
    field is additive — pre-#4890 clients decode both responses unchanged.
  - `affirmed_at` is **derived** from the active KG row's `valid_from` rather
    than stored as a second column. `assert` already rewrites `valid_from` on
    every assertion, so the value is correct by construction on all 93 existing
    palaces with no migration, and no write path can forget to set it.
    Re-asserting a rule **verbatim counts as re-affirmation** — that is the
    deliberate choice, since retyping a rule is exactly the human review the ADR
    asks for.
  - `trusty-memory doctor` gains a fifth check, "Tier S re-affirmation". It
    names every rule unaffirmed for more than 90 days, its age in days, and both
    the re-affirmation path (`kg_assert`) and the retirement path
    (`remove_prompt_fact` with the row's `subject` and `predicate`) — the same
    way the cap's refusal message names the current 20.
  - **The check never retires anything, and never returns `Fail`.** Promotion
    and retirement of a standing rule are deliberate human acts (D8 point 3); a
    `Fail` would flip `doctor`'s exit code and let a stale rule break a scripted
    health gate, pressuring someone into deleting it unreviewed. A stale rule is
    unreviewed, not broken, so the strongest verdict is `Warn`. When the daemon
    is unreachable the check reports `Unknown`, never `Pass`.
- `trusty-memory backfill-report` — the read-only triage list for ADR-0028's human-gated
  backfill (Migration step 3, closes [#4891](https://github.com/bobmatnyc/trusty-tools/issues/4891))
  - ranks existing drawers by how many turns they actually reached, so the worst
    offenders are triaged first — the ADR's motivating case is a 19-day-old session
    checkpoint reaching 44.8% of turns, and a stale drawer nobody retrieves costs nothing
  - each row carries what a single triage decision needs: drawer id, content excerpt,
    age, injection count and share of that palace's turns, stored importance and its
    decayed value, whether `expires_at` is already set, and room/palace
  - injection frequency is recovered from the enriched-prompt hook logs, which record the
    rendered injection but no drawer id. The join re-renders each drawer's preview with
    the same `drawer_preview` the injection pipeline uses and counts matching bullets;
    against the live estate it reproduces ADR-0028 §C7's table — the top drawer measures
    45.1% of `trusty-tools` turns where the ADR measured 44.8%, and the second measures
    20.7% against its 21.2%. The share is quoted rather than the raw count because the
    count climbs with every logged turn; only the ratio is stable enough to cite
  - two drawers in one palace whose content truncates to the same 220-char excerpt are
    indistinguishable in the logs and receive one combined count. Such rows are marked
    `⚠ SHARED` with a content digest that separates them, and counted in the header, so a
    combined count can never be mistaken for a per-drawer one. The live estate has no
    collisions today
  - a drawer created before the scanned log window carries `predates-log-window`, so a
    reading of 0 injections is distinguishable from "genuinely never retrieved" — the two
    warrant opposite decisions
  - no tier is suggested. §C4 measured why: `resume-target` splits 71/26 across tiers, so
    a tag-derived verdict would be wrong for a quarter of rows while looking exactly as
    confident as the rest. Rows carry checkable observations instead
  - with no hook log present the report says the counts are missing data rather than
    presenting estate-wide zeros as an absence of stale drawers
  - `--json` for scripted triage; `--palace`, `--limit`, `--min-injections`, `--logs-dir`
  - the command writes nothing, by construction: it never opens a `PalaceHandle` (that
    path deletes expired drawers at open, and expired drawers are exactly what a human
    triages), and it reads a private copy of each palace's redb store rather than the live
    file, because `OpenIntent::ReadOnlyClient` only snapshots when the file is already
    locked and otherwise reaches `Database::create` — which runs an init write
    transaction and can rename an incompatible-format store aside
- `palace_reembed` MCP tool — reports drawers that have no vector and optionally re-embeds them ([#4906](https://github.com/bobmatnyc/trusty-tools/issues/4906))
  - defaults to a dry run: it returns the exact set of vectorless drawer ids without touching the embedder
  - `dry_run: false` repairs them; it lives in the daemon because the daemon holds the palace's writer lock, so a CLI would only ever get a read-only snapshot it cannot write vectors into
- `GET /health` reports `unopenable_palaces` — the id and reason for every palace present on disk that startup hydration refused. The key is omitted when there are none, so a healthy daemon's payload is unchanged (#4911).
- `bm25_backfill` — a lossless feeder that indexes a palace's existing drawers.
  The live write path (`bm25_index_enqueue`) holds a 256-slot channel written
  with `try_send` and drops on full, so a backfill routed through it would have
  dropped roughly 80% of the largest palace's 1311 drawers and left it answering
  lexical queries from a fifth of its content. The feeder awaits each document's
  ack instead, so nothing is ever offered to a full queue. Idempotent, bounded
  by a per-op and a per-palace deadline, and it reads coverage back from the
  daemon rather than trusting its own submission count. Runs as a serial startup
  sweep over palaces that have drawers; `TRUSTY_BM25_NO_BACKFILL=1` defers it.
- `palace_unalias`: free drawers whose vector was destroyed by an id collision, so a `palace_reembed` run can make them findable again ([#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - dry-run by default, like `palace_reembed`. It reports the drawer id SET (`freed_ids`), never a bare count — a count-based all-clear is the defect #5005 is about
  - callers branch on `outcome` (`clean` | `planned` | `repaired` | `partial` | `unavailable`) or `success`; `partial` and `unavailable` carry ids and neither is a success. `reembed_required` says outright when a `palace_reembed` run is still owed
  - reports `unnameable_keys`: keys in a collision group that name no drawer, so `aliased_before_ids` can be empty over a real collision. Branch on `outcome`, never on the id counts
  - `palace_reembed`'s tool description now states the guard: `missing: 0` is not a complete account of what is retrievable, so read `alias_audit` — and act only on `is_clean: true` — before deleting a drawer on the strength of that report
- Periodic BM25 coverage repair sweep (`bm25_repair`). The write path drops on a
  full queue, and the only thing that repaired a drop was the next daemon
  restart. A dropped enqueue, a failed index call, and an unverified startup
  sweep now queue the palace, and the sweep re-runs the lossless backfill on an
  interval (`TRUSTY_BM25_REPAIR_INTERVAL_SECS`, default 300s, `0` disables). A
  palace whose coverage is still unverified stays queued.
- The startup sweep enumerates palaces from disk (`list_palaces` + `open_palace`)
  instead of `registry.list()`, which snapshots only currently-open handles and
  is capped at 64. On a ~99-palace host at least 35 were never probed, never
  queued, and the sweep still logged `all coverage verified`. Palaces that
  cannot be enumerated, opened, or verified are now counted and queued, and a
  sweep that enumerated nothing can no longer read as complete.
- The repair pass resolves palaces with `open_palace`, which hydrates. A dirty
  palace that went idle and was evicted was previously dropped from the queue
  permanently, so its gap waited for a restart.
- Sweep enumeration reads palace ids off the data root rather than
  `PalaceStore::list_palaces`, which returns `Ok` while silently dropping any
  palace whose `palace.json` will not decode. Such a palace was absent from the
  count and from the repair queue while the sweep still logged clean; it is now
  seen, recorded as unopenable, and queued.
- A closed BM25 index queue marks its palace dirty, matching the full-queue
  arm. Both lose the write identically.
- **`kg-rebuild --merge-punctuated-twins`** folds a pre-#4678 punctuated entity
  node onto its cleaned twin, so `` `redb` `` and `redb` stop being two nodes for
  one thing. #4678's edge trim fixed extraction going forward and split every
  entity already in the graph; nothing removed the old node, because the four
  pattern predicates are absent from `FUNCTIONAL_PREDICATES` (an `assert` adds
  the cleaned spelling beside the punctuated one) and `--purge-stale-subjects`
  only selects subjects `is_stop_token` rejects, which a real entity never trips.
  Each rebuild over the same drawer content widened the split.

  This is a merge, not a delete: every auto-extracted triple is re-asserted under
  the cleaned identity and only then retracted at the punctuated one, in BOTH the
  subject and the object position, so the merged node keeps its own pre-existing
  triples and gains the re-pointed ones. Object-position re-pointing is what
  needed #5396's `retract_triple` — closing the whole `(subject, predicate)` pair
  would have taken the punctuated object's correct siblings with it.

  Off by default, prints every move, and takes `--dry-run` (which now gates on
  either maintenance flag rather than on `--purge-stale-subjects` alone) to
  report the list while writing nothing. Selection is as narrow as the purge's: a
  term under the `drawer:`/`tag:`/`topic:`/`room:` namespaces never moves, only
  triples stamped `auto:remember` are rewritten, and a triple carrying a
  punctuated stopword at EITHER end is left alone — so `is_stop_token` partitions
  the two passes in the object position too, and the merge cannot hang a `("the`
  off the cleaned node where the subject-selecting purge could never reach it.
  Re-running is a no-op.

  A re-point that cannot write is reported as failed rather than merged and
  exits non-zero, the cleaned node is written before the punctuated row is
  closed so a failure mid-way leaves the fact readable at one node or the other,
  and a data root that cannot be listed fails the run instead of reporting an
  empty one.
- `kg_triple_count_or_zero`'s degrade now has in-crate test coverage ([#5489](https://github.com/bobmatnyc/trusty-tools/pull/5489))
  - The degrade rule moved into its own `triple_count_or_zero(palace, read)` helper that takes the already-performed read as a parameter, so `triple_count_or_zero_degrades_a_failed_read_to_zero` can pin the arm #5384 cares about — a failed count must become a *logged* `0`, never a silent one. `KgStoreRedb::db()` is `pub(super)` to `trusty-common`, so the failure cannot otherwise be induced from this crate, and the error arm was unexecuted here.
  - Behaviour at every call site is unchanged.

### Fixed

- `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` closes the one triple named, not every object at its `(subject, predicate)` pair.
  - The id encoded only `subject + "\0" + predicate` and the service called the pair-level retract, so deleting `alpha is thing-a` also closed `alpha is thing-b`. Retraction is a soft close, so triples lost this way are still readable through `dump_all_triples`.
- Retracting a hot-predicate triple over HTTP rebuilds the prompt cache, matching the `kg_retract_triple` MCP tool. Previously a retracted Tier S fact kept being injected until the next write.
- **`kg_list_subjects`' tool description told callers two things that stopped
  being true.** It claimed a `kg_query` miss "returns the same empty result as
  an empty graph, so a guess tells you nothing" — but since
  [#5385](https://github.com/bobmatnyc/trusty-tools/issues/5385) a miss carries
  `graph_state: subject_not_found` or `graph_empty` plus a hint. It also
  claimed `truncated: true` means "the page filled to `limit` and more subjects
  may exist" — but since
  [#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810) the handler
  over-fetches one row, so `truncated` is set only when a further subject was
  actually seen, and a page that exactly fills `limit` reports `false`.
- Updated doc comments, `Cargo.toml`, and `README.md` that still named
  `open-mpm` as a consumer of the library-only (`--no-default-features`)
  build path to say `trusty-agents` (renamed in #831).
- two `tools::tests` no longer depend on an environment variable another test
  happened to leak (closes [#4413](https://github.com/bobmatnyc/trusty-tools/issues/4413); refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451)).
  `add_alias_round_trip_through_prompt_cache` and
  `dispatch_discover_aliases_inserts_new_and_dedupes` build `AppState` inline
  (they need `with_default_palace`) and so never ran `test_state()`'s
  `TRUSTY_SKIP_PALACE_ENFORCEMENT` write — they passed under `cargo test` only
  because a sibling test in the same process had set it first. In isolation they
  verified nothing, failing at `palace_create` before reaching any assertion.
  Both now call a named `skip_palace_enforcement()` helper, which
  `test_state`/`test_state_warming` share. `cargo nextest run -p trusty-memory`
  (per-test process isolation) goes from 2 failures to 527/527 passing.
- **`trusty-memory doctor` no longer reports HEALTHY while the daemon is
  wedged (issue #4001).** During the #3992 incident six threads sat parked in
  `concurrent_open::backoff_sleep_ms` with a `memory_remember` hung ~1800 s,
  and doctor reported healthy throughout — it checked HTTP liveness, fastembed
  cache state, and lock-file staleness, none of which can observe a wedged
  worker pool. The daemon-health check now reads the `/health` body and fails
  on a reported wedge, naming the age and in-flight count.

- **`trusty-memory doctor` distinguishes "could not determine" from "down"
  (issue #4005).** New `CheckStatus::Unknown`, rendered `❔` and counted in its
  own summary column rather than folded into `passed`. A probe that times out
  now reports Unknown instead of a hard failure, and a 2xx whose body carries
  no worker observation (an older daemon, or an unreadable body) is Unknown
  rather than a pass doctor cannot support. The `/health` probe budget also
  rose from 2 s to 10 s: the default handler samples RSS/CPU behind a mutex and
  enumerates open file descriptors, work the MCP request path never does, so
  under load it could miss a 2 s budget that real traffic never approached.

---
- **`trusty-memory monitor web` no longer reports a healthy daemon as down
  (issue #4635).** `commands::daemon_guard` probed `{base}/api/v1/health` in
  two places — `probe_health()` and the `health_url` field it handed to
  `spin_until_ready` — but `web::router` registers only `GET /health`
  (`src/web/mod.rs:190`). Against a live daemon on :7070, `/health` answers 200
  and `/api/v1/health` answers 404, so the guard always took the cold path: it
  printed `◉ Starting trusty-memory daemon…`, spawned a duplicate daemon (which
  self-exited via `single_instance_check`), then spun on the dead URL for the
  full 30 s startup budget and errored out, leaving the dashboard unreachable
  from its documented entry point. Both probe sites now derive the URL from a
  single `health_url()` helper — the drift between them is what allowed the two
  to disagree — matching the `/health` path already used by
  `commands::single_instance`, `commands::start`, and
  `trusty_common::monitor::memory_client`.

- **`trusty-memory port` help text no longer instructs a 404 route
  (issue #4635).** The `--help` example and two `commands::port` doc comments
  told users to `curl http://127.0.0.1:$(trusty-memory port)/api/v1/health`;
  they now name `/health`.
- Bounded the per-palace chat-session store cache, closing an unbounded
  file-descriptor leak that would have hit `EMFILE` in ~3-4 weeks (#4639)
  - `AppState::session_stores` was a `DashMap` with no `remove`, TTL, or cap, so
    every palace the daemon ever touched leaked one `chat_sessions.redb` handle
    for the process lifetime — a live daemon was measured holding 844, all of
    them pinning files already unlinked from disk, growing ~250-300/day against
    an 8 192 fd ceiling
  - it is now an LRU cache capped at 32 resident handles
    (`TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES`), mirroring the `PalaceRegistry`
    LRU that already bounds kg/usearch/recall handles; evicted stores reopen
    from disk transparently on the next request
  - eviction never closes a store a caller still holds, so an in-flight chat
    stream cannot be interrupted and no second open of a live redb file can
    occur
  - `delete_palace` now drops the palace's cached handle, so deleting a palace
    actually releases its fd instead of pinning the deleted inode
- **The palace graph view no longer presents a truncated graph as complete
  (issue #4670).** `GET /api/v1/palaces/{id}/kg/graph` capped `triples` at
  `KG_GRAPH_MAX_TRIPLES` (5,000) while `node_count` / `edge_count` /
  `community_count` in the same payload were computed over the FULL in-memory
  adjacency. On the live `trusty-tools` palace that meant the UI rendered 5,000
  triples under a badge reading "9,311 nodes", with nothing in the response
  indicating a partial view — and because `list_active` orders by `valid_from`
  DESC, the 3,266 dropped triples were silently the OLDEST ones. The payload now
  carries `returned_triple_count`, `active_triple_count`, and a derived
  `truncated` flag, and the graph view's header always states what is rendered
  versus what exists ("75 of 9,311 nodes shown — click a node to expand").
- **KG pattern extraction no longer turns function words into entities
  (issue #4678).** A `PATTERN_TABLE` marker hit (`is a`, `works at`, `uses`,
  `depends on`) took the token on each side as subject and object with no test
  applied, so "calling them is a no-op" asserted `them --is-a--> no-op` into the
  live graph. Tokens are now screened by `is_stop_token`: a closed-class
  function word (article, pronoun, preposition, conjunction, copula, auxiliary,
  discourse adverb) or anything shorter than three characters is refused, and
  refusing either side drops the whole triple rather than half of it. Short
  names this workspace actually discusses — `Go`, `C`, `C#`, `AI`, `KG`, `PR`,
  `CI`, plus the crate aliases `tm`/`ts`/`tc`/`ta` — are allowlisted past the
  length floor, so the filter costs no recall on them. The filter is lexical
  and judges one token at a time, so it does not reach a bad triple whose two
  tokens are both ordinary words (`squash --is-a--> ancestor`); that residue is
  pinned by a test rather than left unstated.
- **Entity tokens no longer keep the punctuation wrapped around them
  (issue #4678).** `first_token` trimmed a TRAILING run of punctuation while its
  doc promised it stripped the leading one, and the set both token helpers used
  omitted backticks, brackets and asterisks — the characters drawer content
  actually wraps names in. So "trusty-memory uses `redb` for persistence"
  asserted the object as `` `redb` ``, a second node for an entity that already
  had one. Both helpers now strip punctuation from both edges through one shared
  `clean_token`, and interior characters are untouched so `no-op`, `c#` and
  `src/main.rs` survive whole.
- palace counts that are UNKNOWN no longer render as a bare `0` (closes [#4682](https://github.com/bobmatnyc/trusty-tools/issues/4682))
  - the /ui Palaces view showed `0 wings / 0 drawers / 0 vectors` directly above a "Drawers (1)" list, because the header badges came from the peek-based `GET /api/v1/palaces` while the drawer list came from a route that opens the palace
  - expanding a palace now also fetches `GET /api/v1/palaces/{id}` (~0.1s) and merges its live counts into that row; `api.getPalace()` had existed and been dead code
  - `monitor palaces <id>` reads `GET /api/v1/palaces/{id}` instead of filtering the bulk list, so its counts no longer depend on whether the palace happened to be LRU-resident
  - wherever the daemon reports `cached: false`, the UI and CLI render `—` (JSON: `null`) rather than `0`
  - the peek-based list route from #4640/#4637 is unchanged — this was entirely caller-side
- The daemon inherits the #4764 panic-safety fix for its disk-size metrics
  ticker, which calls the same shared `trusty_common::sys_metrics::dir_size_bytes`
  walk that self-aborted `trusty-search` 40 times in a week. `trusty-memory` had
  produced no crash reports of its own, but shared the vulnerable code path
  exactly (#4764)
- `trusty-memory` now installs a panic hook at startup that logs the panic
  payload, location, thread, and backtrace through `tracing` before the default
  hook runs, so a future abort arrives with its cause in the log stream rather
  than only as a symbol-mangled macOS `.ips` report (#4764)
- **`kg_query` no longer reports an empty graph when only the subject is
  missing (issue #4775).** Any subject with no active triples got the hint
  "Knowledge graph is empty. Run kg_bootstrap …", including on a graph holding
  thousands of triples — the handler never consulted a whole-graph total, so it
  asserted something it could disprove, and sent callers to re-seed an
  already-seeded graph. The response now always carries `kg_triple_count` (the
  whole-graph active total, on hits and misses alike), and a miss adds a
  `graph_state` of `subject_not_found` or `graph_empty` with the matching hint —
  the `subject_not_found` hint names `kg_list_subjects` so the recovery step is
  a tool call rather than a second guess. A hit carries neither field, so their
  absence means the subject was found. The MCP tool schema is unchanged.
- The test that pinned the old behavior (`kg_query_emits_hint_when_palace_empty`)
  asserted the falsehood and passed: `palace_create` auto-bootstraps at least two
  triples, so the graph it called empty never was. It is replaced by one test per
  outcome, each establishing its own precondition.
- MCP and HTTP now agree on which room a `room` argument names. The MCP tools
  carried their own exact-case, alias-free parser while HTTP used
  `RoomType::parse`, so `room="backend"` stored into `Backend` over HTTP and
  into `Custom("backend")` over MCP — two different rooms with different ids,
  each invisible to the other's filter. There is now one parser
  (`RoomType::parse`), per the common-entry-point rule (ADR-0027 T3).
  - **Behaviour change:** a *new* MCP write with `room="backend"` now resolves
    to `Backend` rather than `Custom("backend")`, and custom room names are
    lower-cased the way HTTP has always lower-cased them. Existing drawers are
    untouched, so a palace can legitimately hold both a legacy
    `Custom("backend")` room and `Backend`; both are now enumerable, and room
    merging is designed but deferred (ADR-0027 D5). Where the legacy room was
    backfilled, the shared canonical key means the new write lands on the
    legacy room's id rather than creating a second one.
- `PalaceInfo.wing_count` stopped reporting a room count (closes
  [#4811](https://github.com/bobmatnyc/trusty-tools/issues/4811), ADR-0027 T8).
  Since the day it shipped, the field rendered as "N wings" in the palace UI and
  the TUI health panel while actually carrying the number of distinct rooms. A
  truthful `room_count` is now reported alongside it, read from the `ROOMS`
  registry so it agrees with `room_list`, and both readers were migrated to it.
  `wing_count` is retained as a deprecated wire field reporting `1` — there is
  exactly one wing (`DEFAULT_WING_ID`) in every palace until ADR-0027 T9 adds the
  Wing entity — and `0` when the palace is not resident, matching the
  unknown-vs-empty contract of every other count on the row.
- `PalaceInfo.wing_count` reports the real number of wings (ADR-0027 T9, [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - #4811 replaced the old room-count-under-a-wing-label with a hardcoded `1`,
    correct while `DEFAULT_WING_ID` was the only wing the model could hold. T9's
    `wing_create` made more than one possible, so the constant became the same
    category of lie the field carried before
  - it is now read from the `WINGS` registry — the same source `wing_list` uses —
    so the number and the tool surface cannot disagree
  - `0` still means unknown for an uncached palace (#4637); an open palace whose
    registry cannot be read degrades to `1`, never to `0`
  - `palace_info` gains `wing_count` alongside the `room_count` #4807 added
- Membership-style knowledge-graph facts are recorded correctly ([#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810))
  - A predicate that names several things — a room's drawers, a project's dependencies — used to keep only the most recently asserted object; all of them are now retained.
  - `/kg/graph`, `kg_gaps`, and neighbour expansion report the real edge set instead of one edge per `(subject, predicate)`.
- Expect `kg_count` totals to RISE on existing palaces after the one-time migration at first open. That is previously-hidden data becoming visible, not a regression.
- Alias and convention predicates (`is_alias_for`, `has_convention`, `is_fact`, `is_shorthand_for`) are unaffected — they remain single-valued, so prompt-fact injection does not grow.
- `memory_recall` and `memory_recall_deep` no longer return the same drawers for every query (closes [#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - the daemon readiness flag was written once, by the startup embedder warm-up; a single failed attempt pinned it at `Warming` for the daemon's whole life, and the MCP recall handlers read it to pick a degraded fallback that ignores the query. Resolving the embedder now clears the flag, and recall consults the embedder itself rather than trusting a flag that cannot be refreshed from the path that reads it.
  - `AppState` no longer carries a second embedder cell independent of `retrieval::shared_embedder()`. Startup latched readiness off one cell while every recall used the other, so the flag described an embedder the request path never touched — and the daemon loaded two ~90 MB ONNX sessions instead of one.
  - `/health` no longer reports `daemon_state: "warming"` on a daemon whose embedder is live — the HTTP path never touches the readiness latch, so that field could contradict the results the same daemon was serving.
- `make deploy` no longer declares `com.bobmatnyc.trusty-memory` canonical — a
  label no host has ever had — nor unloads a `com.trusty.trusty-memory` the
  daemon does not use. The real unit is `com.trusty.memory`. The target now
  defers to `trusty-memory service install`, which evicts old labels under
  launchd with a rollback, instead of a shell `unload`/`load` pair that could
  fail silently and leave the daemon running CLI-detached (#4868)
- `service install` evicts the launchd labels earlier installs registered.
  Removing the Makefile's `launchctl unload` + `rm -f` of
  `com.trusty.trusty-memory` without moving that job into the installer would
  have LOST an eviction this crate already had, leaving a stale unit for a later
  bootstrap to find. `deploy` also boots out the legacy label before
  `cargo install` (#4868)
- Startup palace hydration now opens each palace with the registry's configured `OpenIntent` instead of the zero-arg `PalaceHandle::open` default. `AppState::load_palaces_from_disk` is the path a restarting daemon takes for every palace it already has on disk — the common case — and it was hardcoded to `ReadOnlyClient`, so `main.rs`'s `with_writer_intent()` reached only palaces created after startup. The daemon consequently refused every palace whose store needed the #702 incompatible-format recovery, despite being the writer that owns those files (#4911).
- A palace that startup hydration cannot open is now recorded on the registry rather than only logged. It was absent from the handle cache and absent from `palace_list`, so a refused open read to an operator exactly like deletion even though the bytes were intact (#4911).
- `kg-rebuild`'s applying pass now opens palaces with `OpenIntent::Writer`. It asserts triples through the handles it opens, but built its registry with the default `ReadOnlyClient` intent, so when the daemon held the write lock it silently received a snapshot and every `kg.assert` failed into the non-fatal warn arm — the run reported success having written nothing. It now fails loud against a running daemon instead, matching what the purge pass in the same command already did. The redundant `load_palaces_from_disk` pre-open it replaces also defeated the intent, since that path opens every palace with the zero-arg `PalaceHandle::open`; palace enumeration reads from disk, so nothing observable changes for a rebuild that was already writing correctly (#4911).
- **The `prompt-context` recall query is shaped to the embedder's window instead
  of being cut inside it (closes [#4972](https://github.com/bobmatnyc/trusty-tools/issues/4972)).**
  The raw user prompt went to `/recall` verbatim and `all-MiniLM-L6-v2` truncated
  it at 512 tokens with no warning, no metric and no signal to the caller, so the
  vector represented a prefix. Over the logged corpus 52.0% of prompts exceed
  that window, and 65.3% arrive wrapped in a `<task-notification>` envelope whose
  task ids and absolute paths spend a median 253 of the 512 tokens before any
  payload begins. The hook now strips that envelope to its `<summary>` and
  `<result>` — which alone raises the share of prompts that fit whole from 46.1%
  to 55.3% — and, when the remainder is still over budget, keeps whole leading
  lines (falling back to whole words) rather than letting the cut land mid-word.
  Every reduction is recorded: a `recall_query` object on the enriched-prompt log
  line (`original_tokens`, `sent_tokens`, `sent_tokens_max`, `budget_tokens`,
  `envelope_stripped`, `units_dropped`) plus a `tracing::warn!`. The budget is
  overridable with `TRUSTY_MEMORY_PROMPT_QUERY_TOKENS`; setting it well above the
  real window restores the previous behaviour.

  The token estimate charges ASCII-letter runs 1 token per 2 characters rather
  than per 3. The old divisor was calibrated on English and underestimated every
  compound-word language measured against the model's own tokenizer — Hungarian
  342 against a true 372, Finnish 362 against 392, Dutch 332 against 362 — which
  let those prompts pass through as fitting and be cut inside the embedder while
  the new metric reported no loss. The cost is paid by English: a prompt now
  delivers ~189 true tokens of the 512-token window rather than ~291.

  No divisor above 1 token per character can bound a run of ASCII letters, so
  `recall_query` also carries `sent_tokens_max`, a true ceiling on what was sent.
  `sent_tokens <= budget_tokens` is an estimate clearing a budget, not a proof;
  `sent_tokens_max > budget_tokens` marks the sends whose fit could not be
  proven, so the log stops reporting a clean pass on a query the embedder may
  still have cut.
- **`apiBase()` returned a fragment-bearing string on a hash-routed load.** `computeBase()` in `ui/src/lib/base.js` ran its `$`-anchored strips against the raw `document.baseURI` rather than its pathname, so at `…/#/` the returned base still carried the fragment (closes [#4980](https://github.com/bobmatnyc/trusty-tools/issues/4980))
  - trusty-memory serves its SPA at the ROOT (`src/web/static_assets.rs`), not under `/ui/`, so the `ui/` strip is a no-op here and `apiUrl()` stayed correct — relative URL resolution discards the fragment. Nothing in the SPA calls `apiBase()` directly, so there was no user-visible failure in this crate
  - trusty-search and trusty-analyze mount under `/ui/` and were genuinely broken by the same code; the fix is applied identically here to honour the KEEP IN SYNC contract and to stay correct if memory ever moves to a `/ui/` mount
  - the strips now run against `new URL(document.baseURI).pathname`, re-joined to `origin`; the `window.__MEMORY_BASE__` override branch and the non-browser guard are unchanged
- `Bm25Supervisor` now bounds the daemon population. Nothing but `shutdown` ever
  removed an entry from its map, so one cross-palace `memory_recall_all` over
  ~99 palaces left 99 child processes resident for the daemon's lifetime — and
  each one's memory scales with its palace's drawer text. Two limits now apply
  on every `ensure_running`: a cap on concurrently-live daemons with
  least-recently-used reaping (`TRUSTY_BM25_MAX_DAEMONS`, default 3), and a
  per-daemon RSS ceiling compared against a real measurement rather than merely
  declared (`TRUSTY_BM25_RSS_LIMIT_MB`, default 512, `0` disables). Spawns are
  serialised so a burst fan-out cannot satisfy the cap per-caller and violate it
  in aggregate. An unmeasurable RSS never reaps.
- `palace_reembed` no longer gives a false all-clear for drawers whose vector was overwritten by another drawer's (closes [#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - it now returns `alias_audit` (`clean` | `aliased` | `unavailable`), `alias_audit_error`, `vector_key_rows`, `distinct_vector_ids`, `aliased`, and `aliased_ids` alongside `missing`; `missing` counts drawers with no vector key, and an aliased drawer HAS a key, so `missing: 0` was reported for a palace with four unretrievable drawers
  - a failed audit reports `unavailable` with null in EVERY count-shaped field — `aliased` and `aliased_ids` included, never zeros or empty arrays — a deletion-bearing workflow must require `alias_audit == "clean"` as well as `missing == 0` ([#5000](https://github.com/bobmatnyc/trusty-tools/issues/5000) resolution item 3)
- **The BM25 lexical lane addressed the wrong palace, for reads and for writes
  (part of issue #5036).** Two independent keying faults compounded.
  `AppState::bm25_client` was built once as `Bm25Client::for_palace(default_palace)`
  (`src/lib.rs:767`) and holds one fixed socket path, so every search and every
  live index write went to the DEFAULT palace's socket no matter which palace
  was being queried. And the recall handlers passed the palace slug the caller
  REQUESTED while the vector lane used the handle `open_palace` had resolved —
  which differ whenever an alias is involved. `palace_aliases.json` maps
  `bobmatnyc-trusty-tools → trusty-tools`, and the alias directory holds no
  `palace.json`, so `palace_ids_on_disk` (`src/bm25_backfill.rs:667`) never
  enumerates it: the backfill wrote the canonical palace's corpus, reported full
  coverage, and the search read a corpus nobody had written to. On the write
  path the drawer landed in another palace's corpus and the call SUCCEEDED, so
  nothing marked the palace dirty and the repair sweep never ran. Search and
  index now resolve a client bound to the palace's own socket
  (`bm25_client_for_palace`), and every call site keys on `handle.id`.
  `recall_without_embedder` no longer takes a `palace` parameter at all — it
  reads the id off the handle, so the two lanes cannot disagree by construction.
  With `TRUSTY_BM25_DAEMON` unset the lane is off and behaviour is unchanged.
- **`prompt-context` no longer injects drawers that did not match the prompt
  ([#5037](https://github.com/bobmatnyc/trusty-tools/issues/5037)).** The hook asked for `top_k` drawers and rendered whatever came
  back. A probe of "what is the capital of France" against the live palace
  returned five drawers all scoring exactly `0.15` — the L1 no-similarity
  penalty — formatted identically to a genuine `0.56` hit, so the reader could
  not tell noise from signal. Four changes:

  - **Relevance floor.** `RecalledDrawer` now parses the `score` the recall
    endpoint has always sent, and drawers below the floor are dropped via
    `trusty-common`'s `apply_relevance_floor` (default `0.35`; set
    `TRUSTY_MEMORY_PROMPT_MIN_SCORE=0` to restore the old behaviour). It runs
    after the deny-tag filter so a tag-excluded drawer is never also counted as
    withheld.
  - **Withheld notice.** When the floor drops drawers the injection says so and
    points at `memory_recall`, with distinct wording for a partial drop and for
    a recall that kept nothing. Zero candidates still renders nothing — an empty
    palace has nothing to announce. Without this the floor would have swapped a
    visible wrong answer for an invisible one.
  - **Larger budget.** `DEFAULT_TOP_K` 5 → 12 (override ceiling 20 → 30) and
    `INJECTION_BYTE_CAP` 4 KB → 8 KB. Both were sized when nothing distinguished
    a good recall from a bad one, so extra room only bought more noise; with the
    floor active the extra slots can only fill with candidates that cleared it.
    Measured over 80 real logged prompts, `K=12` renders a drawer section of at
    most ~2.7 KB.
  - **Whole-input query, pinned.** The recall query was already the entire user
    prompt; `recall_query_is_the_whole_prompt` now pins that against the
    truncating `hook_prompt_excerpt` helper sitting next to it. Truncation at
    the embedder's 512-token window is a separate layer and remains
    [#4972](https://github.com/bobmatnyc/trusty-tools/issues/4972).
- **The injected drawer preview no longer renders storage provenance or an
  uncapped tag list (closes [#5038](https://github.com/bobmatnyc/trusty-tools/issues/5038)).**
  `compose_injection` walked every tag on every recalled drawer, so
  `creator:client`, `creator:version`, `creator:source` and `creator:cwd` — the
  last a ~90-character absolute path — rendered on each of ~12 drawers per
  firing, alongside a median 17-tag topical list. Measured over 17,176 real
  firings in the enriched-prompt log, the `_(tags: …)_` suffix was **53.0% of
  every byte the hook injected**, outweighing the content preview it labels on
  82.3% of drawers. `INJECTION_BYTE_CAP` never fixed that — it only meant the
  noise evicted real drawers instead of growing the block. Rendering now drops
  the `creator:*` namespace (via `attribution::is_creator_tag`, the predicate the
  TUI and dashboard already hide those tags with) and caps the rest at 4 with a
  `+N more` marker, returning 40.1% of the injection to content. The tags are
  untouched in storage and still returned by `memory_list` / `memory_recall`.
- The rendered tag suffix is now bounded in characters as well as count. The
  4-tag cap was argued from "a label must never outweigh the payload it labels"
  (220 characters) but enforced only a count, and four long hyphenated tags —
  `slate-prioritization-in-flight` and friends, both real tags from the live
  palace — exceed that budget while satisfying the cap.
- BM25 backfill establishes coverage by drawer id, not by document count.
  `stats.doc_count >= drawer_count` was satisfied by a corpus carrying documents
  for drawers the palace no longer has, so `fully_indexed()` returned `true` and
  the startup sweep logged `incomplete=0` over a palace it had never indexed.
  `BackfillReport::fully_indexed()` is now `missing_after == Some(0)` — a
  verified, empty missing set — and no status can satisfy it on its own. A
  coverage probe that could not run reports `None` and logs at `error!`.
- The BM25 supervisor waits 5s after SIGTERM before escalating to SIGKILL,
  up from 2s. The daemon allows itself 2s for its shutdown snapshot flush and
  needs signal delivery, socket cleanup and exit on top, so an equal budget let
  the SIGKILL land inside the flush and lose the open write window.
- Corrected two doc-comment test pointers that named tests which did not exist,
  which failed `scripts/check_test_pointers.sh`. The restart path they pointed at
  now has a real test.
- `memory_forget` now deletes the drawer's BM25 document, so a forgotten drawer
  stops being findable on the lexical lane (#5053)
  - forget removed the drawer from redb and the vector store and never called
    `Bm25Client::delete`, so the drawer's full text stayed in the palace's BM25
    corpus — matching lexical queries, contributing to RRF fusion, and staying
    resident in the daemon's memory and its on-disk snapshot
  - the delete is awaited rather than queued behind the bounded index channel:
    a dropped index request is repaired by the backfill, but the backfill only
    adds, so nothing anywhere would re-attempt a dropped delete
  - when the lane is armed and its daemon cannot be reached, `memory_forget`
    now returns an error naming the drawer instead of reporting `deleted` —
    a caller must be able to tell "deleted everywhere" from "deleted where we
    could look". With the lane off (`TRUSTY_BM25_DAEMON` unset) nothing changes
  - the HTTP `DELETE /palaces/{id}/drawers/{drawer_id}` path deletes the same
    document, since the backfill indexes a drawer however it was written
- BM25 supervisor: a daemon that had stopped serving its socket could be handed
  back to callers instead of respawned. `ensure_running`'s liveness check
  trusted `try_wait()`, which reports whether a child has been reaped rather
  than whether it is serving, so a killed daemon read as alive for the window
  between closing its listener and becoming reapable. Liveness is now backed by
  the socket the caller is about to use, and eviction fires only when the kernel
  proves nothing is listening (ENOENT/ECONNREFUSED) so a busy daemon is never
  mistaken for a dead one. A daemon evicted this way is still running, so it is
  now given a SIGTERM and time to flush its acked writes before the replacement
  takes over its socket, rather than being SIGKILLed on the spot.
- `creator:workstream=` / `ws:` tags resolve against the configured worktree base, so retargeting it no longer silently drops workstream attribution from every memory written afterwards (#5204).
- `memory_forget` returns `status: "not_found"` for a drawer id that does not exist, instead of reporting `status: "deleted"` for a delete that never happened, and no longer emits a `DrawerDeleted` activity event for it. A cleanup loop could report N deletions having made zero (closes [#5231](https://github.com/bobmatnyc/trusty-tools/issues/5231))
  - `DELETE /api/v1/palaces/{id}/drawers/{drawer_id}` answers `404` for an unknown drawer id instead of `204 No Content`, matching the `delete_palace` behaviour established in #180. A malformed id is still a `400`
- 64 tests no longer depend on a mock embedder another test happened to seed
  ([#5378](https://github.com/bobmatnyc/trusty-tools/pull/5378); same defect
  class as [#4413](https://github.com/bobmatnyc/trusty-tools/issues/4413)).
  `retrieval::shared_embedder()` is a process-wide `OnceCell`, so under
  `cargo test` — one process per binary — whichever sibling seeded it first
  satisfied every other test for free. Under nextest's process-per-test
  isolation each test got a virgin cell, reached for the real ONNX model, and
  failed on the HuggingFace download (HTTP 429 in CI), which is what reddened
  CI run 31438217228 shard 1. Every test that embeds now calls
  `seed_shared_embedder_with_mock()` itself — from the shared fixture where one
  exists (`tools::tests::test_state`/`test_state_warming`,
  `web::tests::test_state`, `messaging::tests::fresh_palace`,
  `prompt_context::tests::spin_up_test_daemon_with_palace`,
  `mcp_stdio_tools`'s `Fixture::new`/`seed_palace`) and inline in the eight
  tests that build `AppState` directly. CI reported 17 failures, but that was
  the subset that lost the HuggingFace lottery on one run rather than the
  population; measuring with the embedder made deterministically unavailable
  found 64. With the embedder unavailable, `cargo nextest run -p trusty-memory`
  goes from 655 passed / 64 failed to 719/719, so the suite no longer needs a
  model download at all.
- `kg_query` no longer reports `graph_state: "graph_empty"` when the whole-graph triple count could not be read ([#5384](https://github.com/bobmatnyc/trusty-tools/issues/5384))
  - The count failed open to `0` in `trusty-common`, and #4775's classifier reads `0` as an empty graph — so a redb read failure produced the exact false claim #4775 exists to prevent. `kg_query` now returns the error.
  - `GET /api/v1/palaces/{id}/kg/count` answers 500 instead of `{"active": 0}`, and `kg_graph`'s `truncated` flag no longer computes against a `0` that would make every payload look complete.
  - The status, console-metrics, and palace-info roll-ups still degrade to `0` — they have no field for "unknown", per #4637 — but do it at a single named call site (`kg_triple_count_or_zero`) that logs the palace and the error. The chat prompt prints `unknown (read failed)` rather than a `0` the model would repeat.
- KG pattern extraction now takes the head noun of a noun phrase rather than the
  token nearest the marker, using part-of-speech membership from a vendored
  WordNet 3.1 projection. `match exhaustiveness is a hard requirement` yields
  `exhaustiveness --is-a--> requirement` instead of the adjective `hard`, and
  `the squash is an ancestor of origin/main` yields nothing instead of
  truncating a relation into the type `ancestor` — the residue #4678 could not
  reach (#5399).
- A phrase terminator hidden behind markdown emphasis no longer lets extraction
  run into the next sentence: `**MCP is a thin proxy.**` asserted
  `mcp --is-a--> sessions` because the raw token `proxy.**` ends in `*` and only
  its last character was checked.
- The KG noun-phrase walk no longer crosses a line break. Production hands the
  extractor whole multi-line drawer bodies (`memory_remember`, `kg-rebuild`), so
  a walk with no newline boundary took its head from the next sentence:
  `trusty-search is a daemon\ncargo builds it` asserted
  `trusty-search --is-a--> builds`. Because the KG keeps one active triple per
  `(subject, predicate)`, a rebuild rewrote a correct stored object with the
  wrong one (#5399).
- A participle no longer heads a noun phrase. `WordNetPos::mask` now retries a
  regular `-s` / `-es` / `-ies` / `-ing` / `-ed` inflection against its base
  form, so `containing` resolves to the verb `contain` and
  `Each skill is a directory containing:` yields `skill --is-a--> directory`
  instead of `containing`. Plurals keep their noun sense, so `parsers` is still
  a valid head (#5399).
- `inside` and `outside` join the closed-class preposition list; WordNet records
  both as nouns, so without it they passed the part-of-speech check and
  continued a phrase they actually close. This is what ends the phrase in
  `tree is a comment inside crates/…/tests.rs`, which yields `comment` (#5399).
- A plural of a word ending in `e` resolves to that word rather than to the stem
  left by chopping `es` off it. `-es` was tried unconditionally before `-s`, so
  `notes` answered the adverb `not` and `sites` the verb `sit`, both of which
  end a noun phrase — `notes is a drawer` and `sites is a directory` yielded
  nothing at all. The order now follows English spelling: `-es` first only when
  the stem ends in a sibilant, so `attaches` still answers `attach` and not the
  noun `attache` (#5399).
- `kg_triple_count_or_zero`'s doc comment cited `count_active_triples_surfaces_read_failure` as a backtick span, which `check_test_pointers.sh` only resolves within the citing file's own crate — the test lives in `trusty-common`, so the lint reported it dangling. Rephrased the citation to name the crate in prose per the lint's documented convention for cross-crate references; no behavior or test change ([PR #5506](https://github.com/bobmatnyc/trusty-tools/pull/5506))
- **`kg-rebuild`'s registry reads no longer fail open.** `purge_palaces` and
  `rebuild_palaces` both read the palace list through
  `PalaceRegistry::list_palaces(...).unwrap_or_default()`, so a failed read
  became zero palaces and each pass reported a clean, empty run over data it had
  never opened. A macOS TCC denial on the data dir reaches that arm —
  `read_dir` returns EPERM — so the exit-0 was reachable, not theoretical. Both
  now propagate the registry error with the path they could not read.

  What each one was costing differs, and the fix is worth the same weight for
  different reasons. `--purge-stale-subjects` is destructive: it printed a
  zero-count summary and exited 0 while the operator believed subjects had been
  deleted, and the next thing they would do is trust that the graph was cleaned.
  The plain back-fill only asserts, so nothing was destroyed — it just reported
  `0 drawers, 0 triples` about a rebuild that never opened a palace, and an
  operator watching for a triple count would see a real-looking zero.

  This completes the pattern #5401 started in `merge_palaces`; all three passes
  over the palace registry now fail the run rather than reporting an empty one.
- A standing rule written over HTTP or from chat now reaches the prompt context instead of being stored and ignored (closes [#5524](https://github.com/bobmatnyc/trusty-tools/issues/5524), closes [#4905](https://github.com/bobmatnyc/trusty-tools/issues/4905))
  - `POST /api/v1/palaces/{id}/kg` and the chat assistant's `kg_assert` tool both
    stored a hot-predicate triple and never rebuilt the prompt cache, so the fact
    stayed invisible to every later turn while both surfaces reported success.
    Which client a user happened to reach for silently decided whether their fact
    took effect
  - the cause was duplication, not two independent oversights: six surfaces each
    carried their own copy of the admission → assert → refresh sequence, and the
    refresh step drifted out of two of them. All six now route through one entry
    point, `kg_write::assert_triple`, so a new write surface gets the whole
    sequence by construction and a behaviour fix lands once
  - a failed prompt-cache rebuild is now reported as `KgWriteError::CacheRefresh`
    instead of being swallowed as a `warn!` on four separate paths. "Stored but
    invisible" is the exact defect this change removes, so it is no longer a
    condition a caller can mistake for success. Behaviour change: an HTTP assert
    whose rebuild fails answers 500 rather than 204 — the triple is in storage
    either way, and the arm is unreachable today (see below)
  - the Tier S admission gate (#4888) is unchanged in effect on every path, and
    the refusal text still names the occupying facts and `remove_prompt_fact`
  - known limitation, unchanged by this fix and now documented at the call site:
    `gather_hot_facts` logs and skips a palace it cannot read, then returns `Ok`.
    A transient read failure therefore truncates the rebuilt cache — and the
    Tier S occupancy count that gates admission — without reporting anything
- `kg-rebuild` no longer prints `[ok]` for a palace whose triple assertions all failed. `rebuild_one` hardcoded `error: None`, so every failed `kg.assert` lived only in a `tracing::warn!` line and the summary read the same whether all triples landed or none did — the state a running daemon produces, since a read-only snapshot open rejects every assert. Failures are now counted and the summary carries `N of M extracted triple(s) failed to assert; first: <subject> --<predicate>-->: <error>` (#5531).
- The HTTP surface no longer answers 404 when it cannot determine whether a palace exists. Both `open_handle` helpers — `web::error::open_handle` and `MemoryService::open_handle` — mapped every `PalaceRegistry::open_palace` failure to "palace not found", so a denied or transient read of `palace.json`, undecodable metadata, an open-queue timeout, or a redb write-lock conflict all told the client the palace does not exist. A genuine absence is still 404; anything else is now 500 and says the palace could not be loaded. This reaches every `/api/v1/palaces/{id}/kg*` route, the drawer CRUD and per-palace recall routes, `/api/v1/kg/gaps`, `/api/v1/kg/aliases`, and the three `/api/v1/messages` endpoints (#5549, ADR-0045).
- `PATCH /api/v1/palaces/{id}` no longer answers 404 when it cannot determine whether the palace exists. Both rename paths mapped every `PalaceStoreError` through `not_found`, so a denied or transient stat of `palace.json` told the client the palace does not exist — erasing, at the caller, the distinction `load_palace` draws. A genuine absence is still 404; anything else is now 500 and says it could not load the palace (#5549, ADR-0045).
- The startup migration `migrate_default_palace_name` no longer skips silently when it cannot stat `localLLM/palace.json`. Its `exists()` pre-check read a denial as "no default palace on this host" and no-op'd every boot without reaching the propagation below it; the pre-check is gone and only a genuine `NotFound` is a no-op (#5549).
- Stopped the doctor `checks` module doc from linking to the macOS-gated
  `check_launchd_plist`, which rustdoc cannot resolve on Linux. docs.rs builds
  on Linux once per release and never rebuilds.

### Performance

- **`GET /api/v1/status` and `GET /api/v1/palaces` no longer force-open every
  palace on disk (issue #4637).** Both handlers looped over the whole registry
  calling `PalaceRegistry::open_palace` — a synchronous, blocking per-palace
  load (usearch vector index, KG redb, full drawer table, recall-log redb) —
  inline on the async axum executor, with no `spawn_blocking`, no timeout and
  no pagination. Against the live daemon's 5,794 palaces and the 64-slot LRU
  (`DEFAULT_MAX_OPEN_PALACES`), ~98.9% of those were cold opens at ~0.9–1.1 s
  each: roughly 87–106 minutes of disk I/O per request. Measured before the
  fix, `/api/v1/status` did not respond within 90 s while `/health` returned in
  36 ms. Both routes now read counts through `PalaceRegistry::peek` — a
  cache-only, zero-I/O, non-promoting lookup — carrying the fix from issue
  #1924 (which fixed the same anti-pattern in the MCP `console_metrics`
  handler) across to the HTTP handler path, where it had never been applied.
  The same conversion lands on the chat-surface twins `list_palaces` and
  `get_status`, and the `PalaceRegistry::list_palaces` directory walk that
  feeds them now runs on the blocking pool.

- **Cross-palace recall and the dream cycle no longer park a tokio worker
  thread (issue #4637).** `recall_all` (`GET /api/v1/recall`), its chat and MCP
  twins, and `dream_run` (`POST /api/v1/dream/run`) genuinely need every palace
  open — a recall answered from cache-resident palaces only would silently omit
  ~98.9% of the corpus, and a dream cycle that skipped uncached palaces would
  silently stop maintaining them — so these are deliberately *not* converted to
  `peek()`. Their blocking open loops moved to `spawn_blocking` instead, which
  keeps the semantics intact while taking the multi-minute stall off the async
  executor. Three byte-identical copies of that loop collapsed into one shared
  `open_palaces_blocking` helper. These routes are still slow by nature; making
  them fast needs a cross-palace index or an explicit palace scope, not a
  cache-only read.

### Changed

- **`/api/v1/status` totals now cover cache-resident palaces only, and say so
  (issue #4637).** `total_drawers`, `total_vectors` and `total_kg_triples` are
  summed across the palaces resident in the open-handle cache rather than every
  palace on disk — that is what makes the endpoint answer at all at 5,794
  palaces. `palace_count` is unchanged and still reports the true on-disk
  total; a new `cached_palace_count` reports how many of those the three totals
  actually cover. The chat-surface `get_status` tool gained the same field.

- **`/api/v1/palaces` rows carry a `cached` flag (issue #4637).** When `cached`
  is `false` the row's `drawer_count` / `vector_count` / `kg_triple_count` /
  `wing_count` / `node_count` / `edge_count` / `community_count` are `0` because
  they are *unknown*, not because they are empty — the list route no longer
  opens a palace just to count it. Fetch `GET /api/v1/palaces/{id}` for live
  counts on a specific palace; the single-palace route still opens it and is
  unaffected. Both new fields are `#[serde(default)]`, so older clients that do
  not know them are unaffected.
- The `disk_bytes` health metric is recomputed every 60 s instead of every 10 s,
  matching `trusty-search`. Walking the data root six times less often cuts
  exposure to the concurrent-mutation race behind #4764 by the same factor, at
  no user-visible cost for an at-a-glance footprint figure (#4764)
- `LAUNCHD_LABEL` is read from `trusty_common::launchd_labels::MEMORY` rather
  than restated. The value is unchanged; a correct-but-duplicated literal is the
  state trusty-search's label was in before it drifted (#4868)
- the `list_drawers_creates_desc_paginates` fixture builds its drawers with `Drawer::new` plus field assignment instead of a struct literal (refs [#4884](https://github.com/bobmatnyc/trusty-tools/issues/4884)). The literal named all twelve `Drawer` fields while the test only cares about `importance` and `created_at`, so every field added to `Drawer` in trusty-common broke this crate's build for no behavioural reason — `fact_key` was the third such break. No production code or behaviour changed.
- `Bm25Supervisor` is now a thin face over `trusty-common`'s
  `uds::supervisor::UdsServiceSupervisor` (#5089). Its public API is unchanged —
  same constructors, same `ensure_running(palace, data_dir)`, same counters —
  and every behaviour it had is preserved by the shared implementation. What
  stays here is what is genuinely BM25's: the `TRUSTY_BM25_*` knobs, the socket
  path convention, the daemon's argv, and its two timing numbers. Those two are
  now passed in as a `ServiceTimeouts` value rather than being module constants:
  the 3 s spawn budget is justified by BM25 having no model to load, and the 5 s
  SIGTERM patience by the daemon's own 2 s `SHUTDOWN_FLUSH_TIMEOUT`. The
  compile-time guard on that relationship survives as the `BM25_TIMEOUTS` const
  item — `ServiceTimeouts::new` is a `const fn` that asserts it, so lowering the
  patience below the daemon's flush budget still fails the build
- The daemon binary is now located lazily, inside the spawn-spec closure, so the
  external-mode, already-running and socket-adoption paths no longer require
  `trusty-bm25-daemon` to be installed at all
- Unit coverage for the shared machinery — the LRU cap, the RSS ceiling, the
  three-state socket probe, the eviction bookkeeping — moved to
  `trusty-common`'s `uds/supervisor/tests.rs` with the code it covers. Every
  assertion has a counterpart there; nothing was dropped
- The MCP tool section of `README.md` is now generated from
  `tool_definitions()` by `tests/generated_docs.rs`. It replaces five
  hand-maintained category tables that between them listed 20 of the 45 tools,
  with a complete roster and a derived count. Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-memory --test generated_docs` (#5205)
- `tool_definitions_lists_all_tools` no longer asserts a hardcoded tool count.
  The count is derived from the same function the README renders from, so the
  test now asserts its hardcoded roster and the served set are exactly equal in
  both directions instead (#5205)
- The `serve --stdio` bridge now starts the HTTP daemon when it is not running
  instead of hard-erroring with "run `trusty-memory start`". The start is
  single-flight: it takes an exclusive `flock` covering probe, spawn, and
  readiness wait, so N concurrent bridges produce exactly one daemon (#1152).
  `trusty-memory start` now waits for the daemon to answer `/health` before
  returning, which is what keeps that exclusion honest.
- BM25 lexical recall runs in-process. The `trusty-bm25-daemon` per-palace subprocess, its bundled `[[bin]]` shim, and `Bm25Supervisor` are gone; `bm25_lane::Bm25Lane` owns an LRU-bounded map of per-palace `bm25_index::PalaceBm25Index` values instead, the same way trusty-search has always run this index. `cargo install trusty-memory` produces one fewer binary. Existing snapshots are read in place — the path (`<data_root>/<palace>/bm25/bm25_index.json`) and the JSON format are unchanged, so no migration step is needed and a downgrade still reads what this writes. The gate keeps its name, `TRUSTY_BM25_DAEMON=1`, so an operator who set it does not silently lose the lane. Two env knobs were renamed with the thing they bound: `TRUSTY_BM25_MAX_DAEMONS` became `TRUSTY_BM25_MAX_PALACES` (resident indexes, default 3) and `TRUSTY_BM25_RSS_LIMIT_MB` became `TRUSTY_BM25_TEXT_BUDGET_MB` (retained corpus text, default 512). `TRUSTY_BM25_EXTERNAL` is gone — there is no external process to defer to. What this costs: a runaway BM25 index can no longer be SIGKILLed independently of the recall path. Closes #5329.
- `AppState`'s `bm25_client` and `bm25_supervisor` fields are replaced by a single `bm25: Option<Arc<Bm25Lane>>`, and `with_bm25_client_from_env` is now `with_bm25_lane_from_env`. `BackfillStatus::DaemonUnavailable` is now `IndexUnavailable` — same meaning to a caller, different cause. `tools::Bm25IndexRequest` loses its `data_dir` field; the lane derives the path.

### Documentation

- `kg_triple_count_or_zero`'s `Test:` pointer now names its cross-crate coverage in prose ([#5489](https://github.com/bobmatnyc/trusty-tools/pull/5489))
  - The citation of `count_active_triples_surfaces_read_failure` sat in the leading backtick run, where `scripts/check_test_pointers.sh` resolves names only against the citing crate. The test is real and lives in `trusty-common`, so the pointer lint read it as dangling and went red on main.
- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- **Module docs render once instead of twice.** 14 modules carried both an outer `///` on their `mod x;` declaration and their own inner `//!`; rustdoc concatenates the two, so each module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - six modules had the outer merged forward rather than deleted, because it held a fact the inner did not: `dream_scheduler` (the `make_shutdown_watch` / `spawn_shutdown_bridge` exports), `idle_evict` (wired in `spawn_startup_tasks`), `events` and `http_server` (issue #1195, and the crate-root re-export that keeps `trusty_memory::DaemonEvent` paths working), `foreground` (launchd needs loud failure on port collision, not silent port-walking), and `wordnet_pos` (the `NOUN`/`VERB`/`ADJ`/`ADV` bitmask constants)
  - `palace_id_derive`'s outer doc was stale — it still described the pure derivation core that moved to `trusty-common` in #1605 — so deleting it removed wrong information rather than a duplicate

## [0.22.0] — 2026-07-27

MINOR, not the patch 0.21.3 this was originally staged as (#4177). This crate
publicly re-exports `trusty-common` items — `src/palace_id_derive.rs:19`:

```rust
pub use trusty_common::palace_id::{
    derive_palace_id, owner_repo_from_git_remote, palace_override_from_env,
    parent_dir_slug, PALACE_OVERRIDE_ENV,
};
```

The module is declared `pub mod palace_id_derive;` at `lib.rs:140` with no
`cfg` gate and no feature guard, so the whole shim is unconditional public API.
Raising the `trusty-common` requirement from `^0.26.2` to `^0.27` changes the
identity of publicly re-exported items, which at patch level would let `^0.21.2`
re-resolve already-published consumers onto the new identity — the same shape
that forced the trusty-analyze 0.7.3 yank. `^0.21` excludes 0.22.0, so
published consumers keep resolving to 0.21.2 and stay installable.

Consumer pin updated in the same change: `trusty-agents` required
`trusty-memory = "0.21.1"`, i.e. `^0.21.1` = `>=0.21.1, <0.22.0`, which 0.22.0
does **not** satisfy — left alone it would have traded one red for another. It
is now `"0.22"`. `trusty-agents` is unpublished (0.38.6), so this is a
requirement edit only and its own version is untouched.

### Changed

- `trusty-common` requirement raised to `^0.27` (was `^0.26.2`): 0.27.0 makes
  `ChatEvent` `#[non_exhaustive]`, which a `^0.26` requirement cannot express.
  Because the re-exports above are public, this requirement change is itself
  the reason for the MINOR level.

### Fixed

- **Post-publish source drift — the SSE chat handler did not compile against
  the `ChatEvent::Usage` variant.** `src/chat/handler.rs` gained its
  `ChatEvent::Usage(_)` arm in #4112, *after* 0.21.2 was published, so the
  published 0.21.2 artifact cannot build against any `trusty-common` carrying
  that variant. This release ships the arm. The match also gained a wildcard
  arm now that `ChatEvent` is `#[non_exhaustive]`, so the next variant
  addition is no longer breaking here.

---

## [0.21.2] — 2026-07-26

### Fixed

- slim build (`--no-default-features`) now compiles: `tools::dream_ops` reached
  the user-config loader through the `axum-server`-gated `crate::web::` re-export,
  breaking any dependent that opts out of `axum-server` (e.g. `trusty-agents`,
  which uses `default-features = false`). Now routes through the axum-free
  `crate::service::load_user_config` like `chat_provider()` already does
  (closes #2049).
- decouple recall/remember from embedder warm-up (closes #1970) ([#1972](https://github.com/bobmatnyc/trusty-tools/pull/1972)) ([`bb322d4`](https://github.com/bobmatnyc/trusty-tools/commit/bb322d4678f8e167691688e77190b44d9c08627a))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- stop console_metrics force-opening every palace on poll (closes #1924) ([#1926](https://github.com/bobmatnyc/trusty-tools/pull/1926)) ([`74e9e54`](https://github.com/bobmatnyc/trusty-tools/commit/74e9e54243efc6de3778d7c43d938add2ab7b676))

---

## [0.21.1] — 2026-07-24

### Fixed

- **`trusty-memory service install` wrote the LaunchAgent plist but never
  loaded (bootstrapped) it** (#3832, demo-critical): every sibling daemon's
  `service install` (`trusty-search`/`trusty-analyze`/`trusty-review`) both
  writes AND loads the agent in one step, and `trusty-installer`'s
  post-install bootstrap step depends on that uniform contract — it shells
  out to `<binary> service install` for every launchd-managed member and
  treats a clean exit as "installed and bootstrapped". trusty-memory alone
  split "install" (write-only) from "start" (write+load), so a fresh-machine
  `tctl install` reported success while `~/Library/LaunchAgents/com.trusty.memory.plist`
  sat on disk unbootstrapped and absent from `launchctl list` — and the
  installer's later `launchctl kickstart -k` recovery retry then failed
  outright (kickstart cannot force-start a label that was never bootstrapped),
  surfacing as a bare, undiagnosed `down` with no `(kickstarted)` qualifier.
  `service install` now calls `LaunchdConfig::bootstrap()` after writing the
  plist, exactly like its siblings; a bootstrap failure now propagates as an
  `Err` (never swallowed) instead of being silently skipped. `service start`
  is kept as an idempotent alias of `service install` for backward
  compatibility with existing scripts/docs. Verified manually (`cargo run -p
  trusty-memory -- service install` + `launchctl list`); there is no trait
  seam in `trusty_common::launchd::LaunchdConfig` yet for unit-testing
  install-then-bootstrap sequencing in isolation — tracked as a follow-up in
  `trusty-installer`'s CHANGELOG. `trusty-installer` 0.4.8 adds an
  independent installer-side defensive fallback (verifies `launchctl list`
  itself and force-bootstraps if needed) so this fix is not the only thing
  standing between a demo and #3832 recurring.
- **`serve --stdio --palace <default>` never reached real MCP tool calls**: `inject_default_palace` (`commands::serve_stdio_bridge`) only wrote the default into top-level `params.palace`, but a real MCP client (Claude Code) sends the standard `tools/call` envelope (`method: "tools/call"`, `params: {name, arguments}`) and tool handlers read `arguments.palace` — so every real `tools/call` request reached the handler with no palace at all, surfacing as `-32603: memory_recall: missing 'palace' (no --palace default configured)` even with `--palace` supplied on the CLI. `inject_default_palace` now mirrors the sibling `inject_caller_context`'s dispatch-shape branching: it injects into `params.arguments` for `tools/call` requests, and keeps the pre-existing top-level `params.palace` injection for legacy direct method-per-tool requests. Caller-supplied palace values are never clobbered either way.

## [0.21.0] — 2026-07-23

Folds in the never-published 0.20.0 (see below — version bumped in source but
no tag/crates.io release was ever cut for it) plus the new DOC-53 workstream
attribution work.

### Added

- **Workstream-attributed drawers** (DOC-53, part of the workstream claim-drawer coordination convention): `crates/trusty-memory/src/attribution.rs`'s `creator:*` namespace gains `creator:workstream=<name>`, plus a bare `ws:<name>` tag for ergonomic `memory_list`/`memory_recall` filtering — both rendered by `CreatorInfo::into_tags()` alongside the existing four attribution tags, and both omitted cleanly (no placeholder) when the workstream name isn't resolvable. New `X-Trusty-Client-Workstream` HTTP header and MCP `args["workstream"]`/`args["cwd"]` fields (mirroring the existing `args["cwd"]` precedent on `palace_create`) let a caller self-report its identity; the MCP stdio bridge (`commands::serve_stdio_bridge`) auto-injects its own resolved identity into every forwarded request, mirroring the existing `--palace` default-injection mechanism.

### Fixed

- **Daemon-vs-caller mis-attribution in DOC-53's workstream stamping** (code-critic BLOCK round, same day as the feature landed): the initial cut resolved workstream identity via `CreatorInfo::new_self`, which reads `std::env::current_dir()`/`TM_WORKSTREAM_NAME` from the *daemon process itself* — since `trusty-memory` serves every concurrently-attached MCP/HTTP session from ONE shared process, every caller's writes were stamped with the SAME (daemon's own) identity, a worse failure mode than no attribution (a plausible-looking-but-wrong tag). `tools::helpers::attach_mcp_attribution` and the HTTP path (`web::rpc::creator_info_from_http`) now use a new `CreatorInfo::new_for_caller` constructor that trusts ONLY per-request caller-supplied `cwd`/`workstream` (from MCP `args` or the new `X-Trusty-Client-Workstream`/`X-Trusty-Client-Cwd` HTTP headers) and never falls back to the daemon's own env/cwd; `CreatorInfo::merge_into_deduped` prevents a hand-written claim drawer's own `ws:<name>` tag (DOC-53 §3.1) from being duplicated by the auto-stamp. Regression-tested end-to-end over the real `/rpc` HTTP surface with two simulated concurrent callers (`mcp_writes_carry_distinct_ws_tags_per_caller_over_rpc`).

## [0.19.2] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [0.20.0] — 2026-07-21

### Changed

- **UI tokens now CI-enforced against the canonical Foundry source** (refs [#3486](https://github.com/bobmatnyc/trusty-tools/issues/3486)): flipped from the `scripts/check_token_drift.mjs` allowlist to ENFORCED. The `token-drift` CI job now compares `ui/src/lib/styles/tokens.css`'s plain-CSS `--trusty-*: #hex` values directly to `docs/design/UI/design-system/tokens.css` on every push/PR (light `:root`, dark `[data-theme='dark']`), so a hand-edit that drifts this crate's palette from canonical fails the build.
- **Migrated the admin UI to Foundry v2 design tokens** ([#3487](https://github.com/bobmatnyc/trusty-tools/issues/3487)):
  `ui/src/lib/styles/tokens.css` now sources its palette, fonts, radii, and
  shadows from the canonical `docs/design/UI/design-system/tokens.css`
  (rust-on-paper light theme) and ships a full `[data-theme='dark']` block
  ("Night Shift") — this UI previously had no dark theme at all. Existing
  `--trusty-*` custom-property names are unchanged; several components that
  referenced tokens the old palette never actually defined
  (`--trusty-bg-subtle`, `--trusty-border-light`, `--trusty-font-mono`, a bare
  `--trusty-text`) now resolve to real values instead of silently falling
  through to their inline fallback. Dark-mode activation follows OS
  `prefers-color-scheme` via a new `lib/theme-bootstrap.js`, wired from
  `main.js` before the shell mounts.

### Security

- **Router-wide same-origin (CSRF) write guard** ([#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)):
  destructive write routes — `DELETE /api/v1/palaces/{id}` (palace deletion),
  `DELETE …/drawers/{drawer_id}` (drawer deletion), `POST /api/v1/admin/stop`,
  `POST /rpc` (the full JSON-RPC tool surface), `POST /api/v1/dream/run`, KG
  asserts/deletes — are now guarded against cross-origin browser requests via
  the shared `trusty_common::server::with_guarded_middleware`. Method-gated (GET
  reads and `/sse` unaffected) and fail-open on a missing `Origin` (the console
  proxy, the `serve --stdio` bridge, and `curl` keep working).

### Fixed

- **`open_activity_log_with_fallback_returns_discard_when_unwritable` no longer mutates the process-global `TMPDIR` env var (issue #3434).** The test used to point `TMPDIR` at an unwritable directory for its duration to force the tempdir-fallback path — but `cargo test` runs every test in this crate's lib binary as threads of ONE process, so any concurrently-running test that called `tempfile::tempdir()` (which respects `$TMPDIR`) during that window failed with `PermissionDenied` for a reason entirely unrelated to its own code. `open_activity_log_with_fallback` now delegates to a new `open_activity_log_with_fallback_in(data_root, fallback_root)` that takes the fallback root as an explicit parameter; the test calls it directly with the unwritable dir, so no env var is touched and no other test can be corrupted by it.
- idle-to-disk palace eviction + unpin dream scheduler + configurable max-open ([#2276](https://github.com/bobmatnyc/trusty-tools/pull/2276)) ([`0e8e504`](https://github.com/bobmatnyc/trusty-tools/commit/0e8e50440cea09a8f5eedf2c7bba9613f96cd8a8))

### Changed

- release trusty-common 0.22.2 + trusty-mpm 0.19.1 ([#2241](https://github.com/bobmatnyc/trusty-tools/pull/2241)) ([`f7ab5f4`](https://github.com/bobmatnyc/trusty-tools/commit/f7ab5f43c8a5cc41ed4d821e2a53800974e74207))

---

## [0.17.0] — 2026-06-25

### Added

- `task_add` MCP tool — creates a `DrawerType::Task` drawer that is never evicted or
  consolidated by the dream cycle (`is_protected() = true`); bypasses content filters
  via `force=true` (spec-001 issue #1722)
- `task_list` MCP tool — returns all Task drawers in a palace; open tasks only by
  default, `include_completed=true` includes tasks with a `completed_at` timestamp
  (spec-001 issue #1722)
- `task_complete` MCP tool — sets `completed_at` on a Task drawer and persists via
  `kg.upsert_drawer`; errors if drawer does not exist or is not a Task drawer
  (spec-001 issue #1722)
- `palace_create` `force=true` flag — bypasses project-slug gate for arbitrary-slug
  palace creation (e.g. per-app/per-tenant chat session stores); slug format validation
  (`[a-z0-9][a-z0-9-]{0,62}`) still runs unconditionally (closes #1719)
- `chat_turn_append` MCP tool — appends a prompt+response pair as two messages (user
  then assistant) to an existing chat session in one call (closes #1720)
- `chat_session_recall` MCP tool — alias for `chat_session_get`; returns ordered turn
  history for a session (closes #1720)
- `chat_session_delete` MCP tool — removes a chat session by ID; idempotent for unknown
  IDs (closes #1720)
- `palace_dream` MCP tool — on-demand, room-filtered LLM compaction; gracefully returns
  a no-op result when `OPENROUTER_API_KEY` is absent (closes #1721)
- chat session manager MVP — force palaces, chat-session MCP tools, room-scoped consolidation, Task drawers (closes #1700, #1701, #1702, #1703) ([#1710](https://github.com/bobmatnyc/trusty-tools/pull/1710)) ([`dcb31f7`](https://github.com/bobmatnyc/trusty-tools/commit/dcb31f7e6743dda227e79cb8d8a7116440868d10))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))

### Fixed

- serialize env-mutating cwd_palace_slug_at tests to stop CI flake ([#1624](https://github.com/bobmatnyc/trusty-tools/pull/1624)) ([`3660bcd`](https://github.com/bobmatnyc/trusty-tools/commit/3660bcd20ca0ff4b726fffce80b846eaa08f2afc))

### Documentation

- correct stale SQLite references to redb in comments and README ([#1704](https://github.com/bobmatnyc/trusty-tools/pull/1704)) ([`63645b3`](https://github.com/bobmatnyc/trusty-tools/commit/63645b3d3028940299dd6f9a4b09310ac5ee5f00))
# Changelog — trusty-memory

## [0.15.5] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-memory` now produces
  `trusty-memory` and `trusty-bm25-daemon` only. The console is its own
  single-owner crate — install it with `cargo install trusty-console`. This
  resolves the cargo binary-ownership collision that forced `--force` on
  install / self-`upgrade` (#1262). `trusty-bm25-daemon` is still bundled here
  (single-owner: memory is its sole producer).

## [0.15.2] — 2026-06-09

### Fixed

- **Lock TOCTOU hardening (#797)** — palace and store operations now acquire
  the advisory lock before any stat/open sequence, eliminating the window in
  which a concurrent writer could observe a partially-written file between the
  existence check and the open.

- **`libc::kill` replaces unsafe `set_var` in tests (#797)** — test helpers
  that previously used `std::env::set_var` (unsound in multi-threaded tests)
  now signal the daemon via `libc::kill`, making the test suite safe to run
  with `--test-threads > 1`. Test isolation improved.

- **Module documentation corrected (#797)** — doc comments that referenced
  internal implementation details now reflect the current architecture.

---

## [0.15.1] — 2026-06-05

### Fixed

- Minor stability fixes after the redb 4.x migration; no user-visible API changes.

---

## [0.15.0] — 2026-06-03

### Added

- **redb 4.x + graceful recovery for activity/store** (#702) — all embedded redb
  stores upgraded to redb 4.x. Existing redb 2.x activity and memory stores are
  detected as incompatible, backed up to `*.v2-incompatible`, and recreated on
  first start.

- **Dashboard auto-start** (#687) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual invocation.

- **add_alias/discover_aliases optional palace param** (#664) — the
  `add_alias` and `discover_aliases` MCP tools now accept an optional `palace`
  parameter to scope alias operations to a specific palace.

- Bundled `trusty-bm25-daemon` as a second binary target. One
  `cargo install trusty-memory` now produces three binaries:
  `trusty-memory`, `trusty-memory-mcp-bridge`, and `trusty-bm25-daemon`.
  Users who set `TRUSTY_BM25_DAEMON=1` no longer need a separate
  `cargo install trusty-bm25-daemon` step.

- `locate_bm25_daemon_binary()` in `trusty-common::bm25_client` (behind
  the `bm25-client` feature flag). Discovery order: `TRUSTY_BM25_DAEMON_BIN`
  env var, sibling of `current_exe()` (bundled-install path), then PATH.
  The `current_exe().parent()` fallback ensures the bundled-install case
  works without `~/.cargo/bin` on PATH globally.

> **OPERATOR NOTE:** Existing redb stores are backed up to `*.v2-incompatible`
> and recreated empty on first start after upgrade.
