# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.13.0] — 2026-08-10

### Breaking

- `ReportSection` and `ReportModel` gain a public `gaps` field (added for #5239's Gaps & Caveats reporting). Both structs are externally constructible, so an exhaustive struct literal built against 0.12.0 no longer compiles — `cargo-semver-checks` flags this as `constructible_struct_adds_field`. Version bumped 0.12.0 -> 0.13.0 per Cargo's 0.x rule (the breaking bump lands in the MINOR position). No in-workspace crate needed a source change: `trusty-analyze` and `trusty-mpm` both reference `trusty-review` via `workspace = true`, so bumping the workspace-level `version = "0.13"` pin in the root `Cargo.toml` was sufficient.

### Added

- `report --analyze` now names every repository it could not enrich, in the report's Gaps & Caveats section, instead of only warning on stderr. A findings table that renders empty because the analyze daemon was unreachable no longer looks like a codebase with no findings (#5239, DOC-67 §9). The fetch contract is unchanged — still fail-open, still never aborts the report.
- `AnalyzeMetricsSource::fetch_named` returns the reason a fetch yielded nothing (`AnalyzeGap::NotIndexed`, `Unreachable`, or `Unavailable`) rather than a bare `None`. It has a default implementation, so existing implementors are unaffected; `HttpAnalyzeMetricsSource` distinguishes the two conditions it can tell apart. The raw transport error stays on stderr — the report gets the category only, so a URL or a response body never reaches an artifact handed to a third party.
- `enrich_with_analyze_gaps` is the enrichment entry point that returns those lines; `enrich_with_analyze` is unchanged and now delegates to it.
- A manifest may declare `[report] gaps = [...]` — Gaps & Caveats lines from whoever produced the manifest. `tga audit` uses this to carry the stages its sweep could not complete into the report (#5236).
- `polish_with_gaps` renders declared gaps as their own bullets ahead of the auto-collected `Data gaps:` list. A template with no Gaps & Caveats heading gets one appended rather than dropping the lines.

### Fixed

- A finding the verifier REFUTED no longer leaves the model's own grade standing: the
  post-verification grade is reconciled in both directions, so a relaxed verdict can no
  longer sit beside an `F` that rested on discarded evidence (#4044).
- The narrative summary is written before the verification round and nothing revisited
  it, so it kept citing refuted findings as merge blockers. A deterministic verification
  notice now leads the review body, naming each refuted finding by the index the prose
  uses (#4044).
- A `code_provable` finding whose own text admits the evidence was not available —
  "the diff does not show their signatures, so this cannot be confirmed from the diff
  alone" — is no longer markable `verified: "confirmed"`. It is pre-stamped
  `unverifiable`, stripped of its escalation signals, and never sent to the verifier.
  This generalises #4081's rule from the claim's subject (registry vocabulary) to the
  claim's own admission, which is what let the defect recur (#5309).
- The verifier can now answer `UNVERIFIABLE`. With only CONFIRMED and REFUTED available
  it had to pick one even for a claim the diff cannot settle, and it picked CONFIRMED
  (#5309).
- Refactor suggestions from trusty-analyze no longer render in the report's RED/CRITICAL band, and analyze-derived findings now carry the daemon's own rationale and suggested action instead of showing `not stated in source data` in every prose slot. A finding that would still render as a bare title and path is dropped, and a repository whose analysis ran with no external static-analysis tool installed is named under Gaps & Caveats so an empty RED band reads as unassessed rather than clean (#5317).
- The §7 complexity distribution is fetched from trusty-analyze's new full-corpus histogram instead of being bucketed from a truncated top-1000 hotspot list, so its bands and percentages describe the whole codebase. On a daemon that serves no histogram the table is omitted and the reason stated under Gaps & Caveats, rather than rendering shares of a truncation as if they were shares of the codebase (#5320).

### Changed

- `pipeline::finding_hygiene::HygieneCounts` gained a fourth per-pass counter and is now
  `#[non_exhaustive]`. Construct it with `..Default::default()` from outside the crate.
  Both land in this release together so adding a fifth hygiene pass is not a further
  break (#4088).

## [0.12.0] — 2026-08-10

### Breaking

- `DedupStore`'s `claim`, `complete`, and `release` are now `async` and take
  `&Arc<Self>`; the synchronous forms are renamed `claim_blocking`,
  `complete_blocking`, and `release_blocking`. The blocking forms wait on redb's
  file lock, so calling one from an async task stalls a runtime worker — the
  async forms move that wait to a blocking thread. `DedupError` gains a
  `Contended` variant and is now `#[non_exhaustive]`: an exhaustive `match` on
  it needs a wildcard arm, and in exchange future variants stop being breaking
  changes. Construction of existing variants is unaffected (#5064).
- **`POST /pr/github/webhook` is retired and now returns 404** (#5181, ADR-0034). GitHub deliveries reach `trusty-review` only through `trusty-console`'s `POST /api/webhooks/{source}`, which verifies the HMAC once, spools the payload durably, and relays over UDS to `trusty-review webhook-listen`. The route is deleted rather than stubbed, so a delivery still aimed at it fails visibly at GitHub instead of being acknowledged and dropped. Anyone with a GitHub webhook pointed at `trusty-review` directly must repoint it at the console.
- **The `closed` + `merged` outcome poll is DELETED, not relocated** (#5181, owner ruling). It scheduled a task that slept an hour, which a console-supervised process that exits on SIGTERM cannot honour. Nothing acts on a `closed` or `merged` webhook event any more: the drain records it as deliberately ignored, writes its processed-ledger marker, and removes the entry. `poll_review_outcomes`, `OutcomeStore` / `OutcomeRecord` / `OutcomeError`, `OutcomeConfig`, the `[outcome]` TOML section and `TRUSTY_REVIEW_OUTCOME_ENABLED` / `_POLL_DELAY_MINUTES` / `_DISMISSAL_THRESHOLD` all go with it — a knob an operator could set and get silence from is the same defect one level down. Issue #1421's outcome-feedback loop is unimplemented again and needs a restart-durable design before it returns.
- **Removed public API:** `service::webhook` (whole module), `integrations::github::webhook::verify_webhook_signature` and its re-exports, `integrations::github::outcomes`, `store::{OutcomeStore, OutcomeRecord, OutcomeError}`, `config::{OutcomeConfig, OutcomeFileConfig}`, `ReviewConfig::{outcome, github_webhook_secret}`, and `AppState::shutdown_tx`. The secret and the signature check belong to `trusty-console` alone now (ADR-0034 §3).
- `webhook_listener::run` now takes a `ReviewConfig`, which it needs to build the review pipeline (#5192).

### Added

- `trusty-review webhook-listen` binds `trusty-review-webhook.sock`, the socket `trusty-console` has been relaying verified GitHub deliveries to since #5089 step 3 with nothing on the other end. Each delivery is fsync'd to a durable inbox under the crate's data directory before the acknowledgement is written; an acknowledgement is what lets console delete its own copy, so nothing is acked that is not already held. The listener exits on SIGTERM, so the socket exists without the service running resident. Both the socket and the inbox root resolve from `trusty_common::webhook_relay` rather than being spelled here, so the directory this service writes to is by construction the one `trusty-console` meters for an undrained backlog. The legacy `POST /pr/github/webhook` route is unchanged.

### Fixed

- **`SubprocessAnalyzeClient`'s health check ignored degraded-but-serving,
  permanently blocking the review gate** (closes
  [#4440](https://github.com/bobmatnyc/trusty-tools/issues/4440)).
  - The MCP `review_pr` / `serve` path uses `SubprocessAnalyzeClient`, whose
    `health()` probed trusty-search's `/health` and tested `status == "ok"` as a
    literal string. trusty-search latches `status: "degraded"` for its entire
    process lifetime once warm boot skips any index (the underlying
    `degraded_by_timeout` / `degraded_by_tcc` counters are never decremented), so
    `has_analysis()` returned `false` forever and `pipeline::context_gate` skipped
    every review with "trusty-analyze unreachable/not-ready" — against a daemon
    that was up, embedder-ready and answering queries normally.
  - This was a duplicated health check that missed a fix applied to its twin:
    `HealthResponse::serving_state()` already distinguishes "degraded but
    serving" from "not serving" and was applied to the search-side gate under
    #4079. `SubprocessAnalyzeClient` now **consumes** `serving_state()` /
    `is_serving()` instead of re-deriving its own verdict, so one place decides
    what a trusty-search health payload means.
  - The gate is narrowed, not weakened: a genuinely not-serving trusty-search
    (embedder down, or a status that is neither `ok` nor `degraded`) still fails
    the probe as `Unavailable`. trusty-search's own status string is passed
    through verbatim rather than laundered into `"ok"`.
- `service uninstall` removes the unit under its old label too. The owner's host
  has `com.trusty.trusty-review.plist` on disk, so removing only the canonical
  plist left it behind for a later bootstrap to resurrect (#4868)
- `dedup.redb` no longer locks out sibling processes, and a failed claim no
  longer posts an ungated review. The dedup claim store held redb's exclusive
  file lock for the whole process lifetime, so a second opener against the same
  `--log-dir` — `serve --stdio` alongside `serve`, or an ADR-0034
  console-spawned webhook worker alongside the HTTP daemon — got
  `DatabaseAlreadyOpen`, which was downgraded to a warning and run past. The
  store now opens redb per operation and releases it again, waiting out a
  concurrent holder for up to 2s and returning a typed `DedupError::Contended`
  if it never frees; the #702 rebuild-on-unreadable-file recovery is serialised
  behind a lock so two processes cannot rename away each other's database. Every
  `AppState` builder now declares `DedupNeed`: modes that never post do not touch
  the file, and modes that can post fail to start without the claim gate. A
  claim that cannot be established aborts the review instead of proceeding. The
  review is then lost until a human re-requests it — the webhook acks with 202
  before the review runs, so GitHub does not redeliver — which is still better
  than a duplicate comment nobody can retract. An abort releases only a claim
  the aborting review actually acquired, so a failed claim never deletes
  another process's record.
  Store operations run on a blocking thread so the lock wait cannot stall a
  tokio worker (#5064).
- `trusty-review webhook-listen` now drains its webhook inbox into the review pipeline instead of holding acknowledged deliveries forever. Dependencies build lazily on the first actionable delivery and open the dedup store as `Required`.
- The `review_requested` filter moved to `webhook_drain` and is shared with the legacy `POST /pr/github/webhook` route, so the two transports cannot drift on which deliveries cause a review (#5192).

### Changed

- The `require_analyze` skip message no longer tells every operator to run
  `trusty-analyze serve`. That advice was irrelevant in the default subprocess
  mode, where no analyze daemon exists at all; the message now names the real
  preconditions (a serving trusty-search and a runnable `trusty-analyze` binary
  on PATH) and scopes the daemon advice to daemon-backed deployments
  ([#4440](https://github.com/bobmatnyc/trusty-tools/issues/4440)).

---
- The LaunchAgent label moved from `com.trusty.trusty-review` onto the
  `com.trusty.<stem>` convention every loaded trusty-* unit obeys, and is read
  from `trusty_common::launchd_labels::REVIEW` rather than restated beside the
  installer's own copy of it. The old label is recorded as a legacy alias, so an
  install evicts a unit left by a prior version instead of running beside it
  (#4868)
- `service install` routes through the label-correct activation path, so the
  `com.trusty.trusty-review.plist` a prior version installed is booted out and
  deleted rather than stranded. Without that, `plist_label_for` moving to
  `com.trusty.review` would leave `tctl start trusty-review` and `tctl stack
  doctor` pointing at a plist that still exists but is no longer written (#4868)
- `ReviewDeps` derives `Clone`, so one built dependency set can drive repeated reviews (#5192).

### Security

- a repository whose `git ls-files` merely FAILED no longer contributes a `.gitignore`-blind directory walk to the corpus sent to the LLM (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - the walk consults a fixed skip-dir list and nothing else, so a repo with a stale worktree gitlink, `detected dubious ownership`, or an unreadable `.git` handed its ignored `.env` straight to the model; only a CORROBORATED "no repository here" still walks
  - the exit code is not a classifier — git exits 128 for every fatal alike — so the stderr is matched on the parenthesised `not a git repository (or any of the parent directories)` form only, and even that is corroborated against an ancestor `.git` witness before it is believed. `fatal: not a git repository: (null)` (a stale worktree pointer) contains the shorter phrase while meaning the opposite
  - a repository with an EMPTY tracked corpus is now an empty corpus, not a walk trigger: nothing committed yet still means a live `.gitignore`
  - anything git could not answer degrades loudly to no measured baseline rather than a leaky one

## [0.11.0] — 2026-07-27

MINOR, not patch: `HealthResponse::is_serving` and
`pipeline::citation_check::downgrade_uncitable_findings` were removed from the
public API (replaced by `serving_state()` and
`enforce_citation_integrity` respectively), and `pipeline::finding_hygiene` is
a new public module.

### Changed

- `trusty-common` requirement raised to `^0.27` (was `^0.26`, inherited from
  `[workspace.dependencies]`): 0.27.0 makes `ChatEvent` `#[non_exhaustive]`,
  which a `^0.26` requirement cannot express.

### Fixed

- **A hallucinated dependency-version finding wore `code_provable: true` +
  `verified: "confirmed"` and drove a false `BLOCK`/`F`** (closes
  [#4081](https://github.com/bobmatnyc/trusty-tools/issues/4081)).
  - A review of a one-line CI change (`ruff>=0.6.0` → `ruff==0.16.0`) reported
    that `0.16.0` "does not exist on PyPI", reasoning explicitly from the
    reviewer model's training cutoff. It was the current latest release, and all
    ten checks on that PR were green on that exact dependency. Nothing in the
    pipeline performs a registry lookup, so nothing had checked the claim — yet
    it carried both signals a consumer reads to decide a finding is trustworthy,
    and hard-clamped the verdict to `BLOCK`/`F`. A hallucination wearing a
    confirmation is worse than no finding at all, and this failure mode is
    concentrated on dependency bumps — the one diff shape where a reviewer's
    version knowledge is stale by construction.
  - New `pipeline::claim_grounding` pass (wired into
    `finding_hygiene::sanitize_findings`, so both the unified and map-reduce
    paths get it) demotes any finding whose text makes a package-registry
    assertion — version absent, withdrawn, deprecated, "latest release is…",
    "as of my knowledge cutoff" — to the advisory tier: `code_provable` cleared,
    `effort` capped at Medium, `confidence` capped at the advisory band, and
    `verified` pre-stamped with a new `VerifyOutcome::Unverifiable { reason }`.
    `verify::select_candidates` now skips any finding whose outcome is already
    decided, so such a claim is never put to the verifier LLM — a second model
    with the same stale training knowledge would only launder the recollection
    into a `"confirmed"`.
  - **Refuse-to-confirm, not look-up.** Querying PyPI/npm/crates.io was
    considered and rejected: it buys nothing the reported harm needs, and adds a
    live network dependency, rate limits, and a new way for the review path
    itself to hang. The principle encoded is stronger than any lookup — a claim
    may be marked `verified: "confirmed"` only when something actually verified
    it.
  - **The finding is never dropped.** It still surfaces, verbatim, with an
    advisory note telling the author nothing checked it and to confirm the pin
    themselves. Only its grading weight is removed. A genuine, diff-provable
    finding alongside it keeps full weight and still drives `BLOCK`.

- **A live trusty-search was reported as `"unreachable"`, and a degraded verdict
  was indistinguishable from a complete one** (closes
  [#4086](https://github.com/bobmatnyc/trusty-tools/issues/4086)).
  - **`reachable` now means the transport worked, nothing more.** `probe_deps`
    published one boolean as both `reachable` and `state`, so a trusty-search
    that was up, embedder-ready, and answering queries was reported as
    `reachable: false, state: "unreachable"` whenever its warm boot had left any
    index behind. Since trusty-search #3706 folded the whole
    `warm_boot_degraded` aggregate into the top-level `status`, one unrelated
    repo's index failing to load on the host was enough to trigger it — and
    "unreachable" sends an operator hunting a process that is running.
    `HealthResponse::serving_state()` now returns
    `Serving`/`Degraded(reason)`/`NotServing(reason)` by reading the individual
    warm-boot counters instead of the aggregate flag, a new `DepState::Degraded`
    carries that middle case, and `reachable` covers `Ok | Degraded`. A new
    `detail` field on each dep names the gap and its query-time consequence.
    Top-level `status` still reports `"degraded"` (it gates on `state == Ok`),
    so the fix corrects the misreport without hiding the gap, and the #3693
    benign network-mount watcher-disable path is unchanged.
  - **A degraded review is now detectable from the MCP envelope alone.** A
    `Degraded` result — a real verdict produced WITHOUT full code context —
    returned `isError: false` and no envelope marker, so any consumer reading
    the envelope saw it as identical to a fully-contexted verdict; the caveat
    lived only inside the serialised text blob and a Markdown banner.
    `wrap_result` now emits `mcp_status: "degraded_context"` plus
    `degraded_reason`. `isError` deliberately stays `false` — the review ran and
    its findings are real. A serving-but-degraded search routes to
    `GateOutcome::Degraded` (labelled review) rather than a blanket skip, while
    a search that cannot serve at all still hard-Skips.

- **Reliability cluster: fabricated findings, spec-as-implementation, and
  unsound verdicts** (closes
  [#4042](https://github.com/bobmatnyc/trusty-tools/issues/4042),
  [#4043](https://github.com/bobmatnyc/trusty-tools/issues/4043),
  [#4044](https://github.com/bobmatnyc/trusty-tools/issues/4044)):
  three related emit-time integrity gaps that let trusty-review publish
  claims it could not support.
  - **#4042 — citation verification is now DROP, not downgrade, and covers
    every finding.** The #2882 fix scoped verification to
    `code_provable`/`code:`-cited findings and, critically, exempted the
    mandated `[code: \`path:line\` — "excerpt"]` citation's own excerpt from
    verification — exactly where #4042 showed the model puts its fabricated
    "evidence." `citation_check::enforce_citation_integrity` now verifies
    EVERY finding's `file` against the diff (Kept, SummaryOnly, AND
    Stage-A-dropped files are all indexed so path existence is checkable
    regardless of disposition), verifies `[code: …]` excerpts against their
    own cited path, and adds a same-file/same-line mutual-contradiction
    backstop for findings that assert incompatible content at one locator. A
    finding that fails is DROPPED, never downgraded-in-place.
  - **#4044 — self-withdrawn findings and chain-of-thought leaks.** New
    `finding_hygiene::drop_self_negated_or_leaked_findings` removes any
    finding whose own text already negates it ("Withdrawing…", "The code is
    correct.") or leaks raw mid-reasoning deliberation ("Wait — actually
    looking at the code more carefully…") before it can reach the rendered
    review or the verdict floor. `finding_hygiene::relax_verdict_if_evidence_wiped`
    additionally relaxes a model's own raw `verdict`/`grade` fields to
    APPROVE when hygiene + citation-integrity wiped out every finding this
    run — closing the loop so a BLOCK that rested entirely on
    fabricated/withdrawn evidence cannot survive as the model's untouched
    self-report.
  - **#4043 — spec/doc reported as implementation.** New
    `finding_hygiene::demote_diff_absent_speculation` demotes (never drops —
    the documentation-drift signal is real) any High/Critical finding whose
    own text admits it is reasoning about an implementation absent from the
    diff ("no implementation of…", "if implementation follows the interface
    literally…"), capping it below the BLOCK floor.
  - Known residual scope (disclosed, not fully closed): (a) `[code: …]`
    citation verification checks path+content, not the cited LINE number
    specifically — no hunk-line-number arithmetic is performed; (b) #4043's
    fix is marker-based on the finding's own admission and does not catch a
    finding grounded in an in-code COMMENT's hypothetical (the code file
    itself is real, so extension/path-based detection does not apply there).

---
## [0.10.1] — 2026-07-23

### Fixed

- **`context_gate` no longer treats a degraded-but-serving trusty-search as
  unreachable** (closes
  [#3693](https://github.com/bobmatnyc/trusty-tools/issues/3693)):
  trusty-search 0.38.1 intentionally reports `status: "degraded"` on
  EFS/NFS-mounted repos (file watcher auto-disabled, search itself 100%
  functional), but `context_gate` string-matched `status == "ok"` as its
  reachability test, so every review on such a deployment fail-closed with
  verdict `UNKNOWN`. `HealthResponse` gains `is_serving`, which inspects the
  structured `warmboot_summary.warm_boot_degraded` flag trusty-search
  already reports instead of guessing from the status string: `"ok"` always
  serves; `"degraded"` serves only when `warm_boot_degraded` is false (the
  benign watcher-disable case) and logs a WARN instead of skipping; a
  genuinely broken search (embedder dead, indexes unloadable, or a
  `"degraded"` response with no `warmboot_summary` at all) still fails the
  gate exactly as before. The same swap (`is_healthy` → `is_serving`) also
  landed in the three sibling call sites that share the `probe_deps` helper —
  the HTTP `GET /health` handler, the `review_health` MCP tool, and the
  `console_metrics` MCP tool — so external callers (load balancers, MPM's
  `review_pr` gate, the console dashboard) see the same fix, not just
  `context_gate`'s own internal reachability check. Known residual gap
  (tracked separately, [trusty-search#3706](https://github.com/bobmatnyc/trusty-tools/issues/3706),
  pre-existing and unrelated to this fix): a warm-boot broken only by TCC
  denial, a scan timeout, or mass index loss (no corpus-open failure) still
  reports `status: "ok"` from trusty-search today, so `is_serving` never gets
  to inspect `warm_boot_degraded` for those specific cases either — same
  blind spot the old `is_healthy` had, since both only ever look at the
  top-level `status` string.

---
## [0.10.0] — 2026-07-21

### Security

- **Adopted the shared router-wide same-origin write guard**
  (closes [#3332](https://github.com/bobmatnyc/trusty-tools/issues/3332),
  part of epic [#3328](https://github.com/bobmatnyc/trusty-tools/issues/3328)):
  trusty-review was missed by #3317's guard rollout across
  search/memory/analyze/mpm, leaving the destructive `POST /review` route
  behind permissive CORS with no CSRF defence. `build_router` now applies
  `trusty_common::server::with_guarded_middleware` (router-wide, applied
  after all route registration) with a loopback-only allowlist by default;
  `serve` passes `SelfOrigins::from_bind_addrs` for its resolved bind
  address so a non-loopback bind still trusts itself. The GitHub webhook
  route (`POST /pr/github/webhook`, HMAC-verified via
  `X-Hub-Signature-256`) is unaffected: GitHub sends no `Origin` header, and
  the guard fails open on a missing `Origin`, so webhook delivery keeps
  working unchanged.

### Added

- **Known-sibling port guard extended to `trusty-mpm`'s supervisor metrics
  listener (7881), `trusty-code`'s new default (7882), and `trusty-agents`
  (8080) (#3364).** `default_port_does_not_collide_with_known_siblings` now
  also rejects a future `DEFAULT_PORT` edit that collides with any of these
  — the supervisor entry was previously missing from every sibling's guard
  table, which is how it silently collided with `trusty-code`'s old default.
- `--source-root <dir>` on `run`/`compare`: explicitly point a review's code-context retrieval at an arbitrary checkout instead of relying on `TRUSTY_SEARCH_INDEX`/CWD auto-derivation. If `<dir>` matches a registered trusty-search index (same longest-root-path matching as the existing auto-derive), that index is used. If it doesn't, the review degrades to diff-only mode (no code-context retrieval) with a clear stderr notice and an in-body banner, rather than silently querying the wrong project's index — an ephemeral/ad-hoc index was considered but deferred (interacts with the #2914 ephemeral-index-leak investigation) in favour of this safe default. `TRUSTY_SEARCH_INDEX` remains the fully-explicit override and still wins over `--source-root`; omitting `--source-root` is a zero-regression no-op. `--context-path <glob>` (scoping eligible context paths) was proposed alongside this but is deferred to a follow-up. Closes #2994.
- direct diff ingestion for `run`/`compare`: `--base <REF> [--head <REF>]` reviews an arbitrary local git ref range (`git diff -M <base>...<head>`, three-dot merge-base range, `head` defaults to `HEAD` — never the working tree) instead of fetching a GitHub PR; `--local-diff -` reads a unified diff piped in on stdin. Both new sources behave exactly like the existing `--local-diff <PATH>`: forced dry-run, never post to GitHub. `--base` and `--local-diff` are mutually exclusive (clap-enforced). Closes #2993.
- Per-project `.trusty-review.toml` config discovery and named review templates for the PR-review pipeline (closes #2995). `run`/`compare` now auto-discover `.trusty-review.toml` at the git root of the code under review (same git-root walk as the trusty-search index auto-derive, #661) and read its `[voice]` (`package`, `principles`) and new `[review]` (`template`) keys with precedence `CLI flag > repo .trusty-review.toml > env var > the caller-selected config file` — an explicit `--config` path always skips repo discovery, and behavior is byte-for-byte unchanged when no repo file exists. New `--review-template <name>` flag on `run`/`compare` (plus `[review] template = "<name>"` in config) selects a named markdown addendum, resolved bundled → `<dirs::config_dir()>/trusty-review/templates/review/<name>.md`, mirroring the existing `report` subcommand's template UX. The addendum composes with — never replaces — the existing stock rubric and voice/principles layers, landing last in the layering order (`stock → principles → voice → review template`). Ships one bundled template, `strict-security`. **Security:** both the review-template name and the `[voice]` package name are validated as bare `[A-Za-z0-9_-]+` identifiers — rejecting any absolute path or `..` traversal — at BOTH the config-resolution layer (a rejected repo-sourced value falls through to the next precedence tier rather than propagating) and inside `ReviewTemplateLoader`/`VoiceLoader` themselves (defense-in-depth), since a `.trusty-review.toml` is attacker-controlled (any PR author can add one) and an unvalidated name could otherwise read an arbitrary local file via `Path::join`'s absolute-path-discards-base-dir behavior and inject its contents into the LLM system prompt.

### Fixed

- `--source-root` diff-only fallback (#2994 re-review): the fallback now also nulls out `deps.analyze` (a new `NullAnalyzeClient` mirrors `NullSearchClient`) instead of leaving it wired to the real client against a stale index — `ReviewConfig::resolve_source_root` clears `require_analyze` alongside `require_search` in both the no-match and daemon-unreachable branches. The persisted (degraded) review body now surfaces the actual `--source-root`-specific notice text (e.g. the `Run \`trusty-search index <dir>\`` hint) instead of a generic "trusty-search unavailable" banner. The duplicate stderr notice (logged once via `tracing::warn!` and once via a raw `eprintln!`) is now logged only once.
- **search-unreachable no longer masquerades as a verdict; interactive reviews degrade instead of hard-skipping (closes #3030).** Two related fixes to the required-context gate (#590): (1) an MCP `tools/call` response for a Skip caused by a genuinely unreachable required dependency (trusty-search/trusty-analyze) now sets `isError: true` plus a `mcp_status: "infrastructure_unavailable"` envelope sentinel — previously it came back `isError: false` with the skip buried in the payload's `status` field, indistinguishable from a real gate verdict to a caller that only checks the happy path. A policy skip or a `Degraded` (non-authoritative but real) verdict still returns `isError: false` — only a genuine infra outage gets the loud treatment. (2) `require_search` now resolves per invocation surface when the operator has not set an explicit override: MCP tool calls (`review_pr`/`review_diff`) and CLI `run --local-diff`/`--base`/`--source-root` local reviews default to `InvocationSurface::Interactive` and DEGRADE to a loudly-labelled diff-only review (the LLM still runs) instead of hard-Skipping, since neither surface can post to a real GitHub PR. The GitHub webhook bot and CLI GitHub-PR `run` invocations keep the strict `InvocationSurface::Hosted` default (unchanged, hard-Skip) since they can post a live review. An explicit `require_search` / `TRUSTY_REVIEW_REQUIRE_SEARCH` override always wins regardless of surface. Composes with the `--source-root` diff-only fallback (#2994): a `NullSearchClient` swapped in by the fallback always fails its `health()` probe, so it routes through the same `Degraded` branch (never `Proceed`) regardless of surface — `cmd_run`'s surface selection is now a standalone, unit-tested `surface_for_diff_source` helper (`GitHub` → `Hosted`; `LocalFile`/`GitRange`/`Stdin` → `Interactive`).
- narrowed confidence-banded reconciliation cap for Medium-driven REQUEST_CHANGES/D+ downgrades: when the model's own verdict is a clean APPROVE and the REQUEST_CHANGES floor rests SOLELY on one `Effort::Medium` finding in the marginal confidence band (`FLOOR_MIN_CONFIDENCE` 0.80 < c < the new `SOLO_MEDIUM_ESCALATION_CONFIDENCE` 0.90), the result is now capped at APPROVE* instead — a successor to the #1343 reconciliation cap #1876 removed, scoped narrowly so it does not reopen #1876's under-flagging regression (a ≥2-Medium floor, a solo Medium ≥0.90, any High-effort finding, or a confident conformance divergence all still escalate uncapped). Addresses the Rank-1 driver of the #1897 over-flagging shadow-eval (47% of clean PRs graded BLOCK/F under 0.6.3). Both new thresholds are env-overridable (`TRUSTY_REVIEW_SOLO_MEDIUM_ESCALATION_CONFIDENCE`, closes #1897)
- **BREAKING:** `DEFAULT_PORT` moved from 7890 to 7891 — 7890 collided with
  trusty-embedderd's `--http` mode default. The cross-crate
  known-siblings port-contract test now also tracks trusty-embedderd,
  closing the gap that let the collision through (closes #2573).
  Anyone with tooling, scripts, or a launchd/systemd unit hard-coded to
  `trusty-review`'s old default (7890, e.g. `curl http://127.0.0.1:7890/health`)
  must either pass `--port 7890` explicitly to `trusty-review serve` to keep
  the old behavior, or update that tooling to the new default (7891).
- **`/health` no longer hangs when trusty-search is slow, not down**
  (closes [#3658](https://github.com/bobmatnyc/trusty-tools/issues/3658),
  follow-on to [#722](https://github.com/bobmatnyc/trusty-tools/issues/722)):
  the trusty-search/trusty-analyze dependency probe in the HTTP `/health`
  handler, the `review_health` MCP tool, and the `console_metrics` MCP tool
  had no internal timeout — a memory-pressured (slow, not down)
  trusty-search could hang all three indefinitely, bounded only by the
  clients' own 30 s / 5 s HTTP-transport timeouts. Each dep probe now runs
  under a strict, independent `tokio::time::timeout` (default 2 s,
  configurable via `TRUSTY_REVIEW_DEP_PROBE_TIMEOUT_SECS`), and both deps
  are probed concurrently so total `/health` latency stays bounded
  regardless of dep count. `DepInfo` gains a new tri-state `state` field
  (`"ok"` / `"unreachable"` / `"timeout"`) so a slow-but-up dependency is no
  longer collapsed into the same `reachable: false` signal as a hard-down
  one; the existing `reachable` boolean is unchanged for back-compat.
  (**Note:** this fix shipped in the 0.10.0 *code* — commit `8f614980`
  merged 2026-07-22 11:18, before the 0.10.0 tag was cut at 14:36 the same
  day — but its changelog entry was mistakenly left under `[Unreleased]`
  instead of being moved here. Backfilled 2026-07-23 during the 0.10.1
  release-prep pass; not re-released under 0.10.1.)

## [0.9.2] — 2026-07-17

### Fixed

- verify `code_provable` finding citations against the actual diff before grading, so a confabulated finding whose cited content is not present in the cited file can no longer fail-close a clean PR (closes #2881). A diff-provable finding that quotes content which lives in a different changed file (the repro attributed a new vitest test file's content to a large regenerated bundle) is downgraded to advisory — its `code_provable` flag and its `source_citation` are cleared and its confidence lowered — instead of driving the deterministic BLOCK / REQUEST_CHANGES floor. Applied on both the unified and map-reduce review paths. The check is fail-open and high-precision: the prompt-mandated inline bracket citations (`[code: … ]`, `[jira: … ]`, `[gh: … ]`, `[apex: … ]`) are skipped before quote extraction so a correctly-cited finding is never false-downgraded, and findings that quote nothing concrete, cite an un-indexed file, or whose quote is grounded in the cited file are left untouched. Residual gap: a fabrication expressed purely in paraphrase (no verbatim code quote) is not caught — the deterministic check requires a concrete quote to verify against. ([#2882](https://github.com/bobmatnyc/trusty-tools/pull/2882)) ([`d7a7459`](https://github.com/bobmatnyc/trusty-tools/commit/d7a7459a704e8b3b6a22418f7eef2b38974e23d9))

## [0.9.1] — 2026-07-16

### Added

- Fireworks.ai as a native LLM provider (fireworks/* routing + constrained structured output) ([#2572](https://github.com/bobmatnyc/trusty-tools/pull/2572)) ([`19d1729`](https://github.com/bobmatnyc/trusty-tools/commit/19d1729bc3061c31166cd2041efec4c2b3e2891d))
- turnkey launchd bootstrap for the shared daemon set (closes #2557, #2556) ([#2566](https://github.com/bobmatnyc/trusty-tools/pull/2566)) ([`f47b428`](https://github.com/bobmatnyc/trusty-tools/commit/f47b4286aeb42d0a5871939edf0019a70cdfab78))

### Fixed

- citable-findings grading rules + citation grammar (fixes PR #84 miscalibration) ([#2825](https://github.com/bobmatnyc/trusty-tools/pull/2825)) ([`805def5`](https://github.com/bobmatnyc/trusty-tools/commit/805def58cc3a08f186b402337e0c722cc01b4a20))
- bump lru and jsonwebtoken to patched versions ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`298df87`](https://github.com/bobmatnyc/trusty-tools/commit/298df87e6a5b5e96874ea2866509303df14712bc))
- bump `jsonwebtoken` 9 → 10 (`aws_lc_rs` backend), fixing GHSA-h395-gr6q-cpjc; migrated the test-only JWT decode-without-verification call site to `jsonwebtoken::dangerous::insecure_decode` (closes #2765) ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`e62b454`](https://github.com/bobmatnyc/trusty-tools/commit/e62b4540d39c5a442d05e849197157932f37e664))
- inject calibration thresholds to fix grade-test parallel-run flake ([#2702](https://github.com/bobmatnyc/trusty-tools/pull/2702)) ([`ed06f1d`](https://github.com/bobmatnyc/trusty-tools/commit/ed06f1dcf4f5b72e43cea18e35165fbac246ead5))

### Changed

- convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))
- mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))

## [0.9.0] — 2026-07-11

### Added

- `report --analyze` — deterministic `complexity_distribution` + RED/AMBER findings from the trusty-analyze daemon, fail-open to scan; balanced severity map ([#2452](https://github.com/bobmatnyc/trusty-tools/pull/2452), epic [#2445](https://github.com/bobmatnyc/trusty-tools/issues/2445)) ([`569a88a`](https://github.com/bobmatnyc/trusty-tools/commit/569a88ab2be33551ed46f6a938160523e8d0c7a7))

### Changed

- Root workspace `[workspace.dependencies]` pin for `trusty-review` bumped 0.8.1 → 0.9.0 (consumed by `trusty-analyze`'s optional `review` feature).

## [0.8.1] — 2026-07-11

### Fixed

- Top Risks table in both bundled templates (`report-technical-dd.md`, `report-technical-dd-cast.md`) hardcapped a fixed number of risk rows (3 generic / 5 CAST), silently dropping synthesized rows 4-5 despite them being present in the JSON twin and within the schema's 5-row max (closes #2373). Converted to a repeatable `BEGIN`/`END` block (same pattern as findings/benchmark rows), rendering one row per synthesized risk with sequential numbering and provenance tags; empty case collapses via omit-empty. ([#2430](https://github.com/bobmatnyc/trusty-tools/pull/2430)) ([`7d844b7`](https://github.com/bobmatnyc/trusty-tools/commit/7d844b7af4a52ed244d5c61c4ed30424c1b2c5f4))

### Changed

- Root workspace `[workspace.dependencies]` pin for `trusty-review` bumped 0.8.0 → 0.8.1 (consumed by `trusty-analyze`'s optional `review` feature).

## [0.8.0] — 2026-07-10

### Added

- **Report wave 2 — analyst instructions + inference-first output** ([#2356](https://github.com/bobmatnyc/trusty-tools/pull/2356)) ([`24d9fb2`](https://github.com/bobmatnyc/trusty-tools/commit/24d9fb2ed2abe967d561cb644e53ebd6904ef1d4)): `--instructions <md>` analyst-brief flag (recorded verbatim + injected as synthesis focus directives); self-derived report metadata (vendor/methodology/dates never honesty-marked); built-in repository scanning (`src/report/scan.rs`) so a bare local-path run produces a measured LoC/file/dependency baseline without an external metrics file; four-kind provenance labels (measured/declared/inferred/not-stated) on every substantive value; omit-empty post-render polish (dropped marker rows, collapsed empty sections, consolidated Gaps list).
- **Report wave 3 — repo-evidence findings investigation** ([#2371](https://github.com/bobmatnyc/trusty-tools/pull/2371)) ([`088b1e7`](https://github.com/bobmatnyc/trusty-tools/commit/088b1e7a17dd096766736b4378e960f3f3e1c72c)): under `--synthesize`, local-checkout repositories are directly inspected — deterministic relevance-ranked file selection, a batched reviewer-role LLM investigation with a verbatim-evidence guardrail (rejects any RED/AMBER finding whose quote doesn't substring-match the real file), a deterministic dependency inventory (manifest + lockfile parsing, no network), a compact findings digest with a one-shot truncation retry so a large verified-finding count can no longer blank the executive summary, and layered section-instruction overrides (generic default → template `<!-- instruct:<section_id> -->` comment → analyst `--instructions` overlay) for the three synthesized sections.
- **Report wave 4 — Mermaid charts from dataset markers** ([#2389](https://github.com/bobmatnyc/trusty-tools/pull/2389)) ([`cc61a96`](https://github.com/bobmatnyc/trusty-tools/commit/cc61a9664f6d616516209b9c97075b05e52c517b)): every populated Graph-Ready Data Appendix table now renders a deterministic Mermaid chart beneath it (`bar`/`stacked-bar` → `xychart-beta`, `radar` → `radar-beta`, `heatmap` → table-only fallback); on by default, disable with `--no-mermaid` or manifest `[report] mermaid = false`; also wires dataset population (e.g. `loc_by_technology` from the wave-2 repo scan) for tables that previously had no fill path on a bare scan-only run.

### Changed

- Root workspace `[workspace.dependencies]` pin for `trusty-review` bumped 0.7.0 → 0.8.0 (consumed by `trusty-analyze`'s optional `review` feature).

### Added

- **Technical-DD report generation** (`trusty-review report --manifest <toml>`) — new manifest-driven report subcommand generating CAST-style technical due-diligence reports (markdown + JSON) from repository inspection:
  - **M1** — deterministic manifest-driven fill: `[report]`/`[[repositories]]` manifest schema (path or remote + username/ref/metrics), git enrichment, v0 trusty-analyze metrics schema, bundled `report-technical-dd` + `report-technical-dd-cast` templates, XDG template override dir ([#2317](https://github.com/bobmatnyc/trusty-tools/pull/2317)) ([`2b916d8`](https://github.com/bobmatnyc/trusty-tools/commit/2b916d8d0cc1e162b8bddcc48d370057d59dc5c6))
  - **M2** — opt-in `--synthesize` LLM narrative synthesis (executive summary, Top Risks, RED/AMBER findings), fail-closed on any provider/parse/guardrail failure, plus a deterministic numeric guardrail rejecting unverifiable figures; GREEN findings never synthesized ([#2324](https://github.com/bobmatnyc/trusty-tools/pull/2324)) ([`f6cbb1b`](https://github.com/bobmatnyc/trusty-tools/commit/f6cbb1bded79d37406ddac37bd28d6feda67b1ff))
  - **M3** — opt-in `--corpus`/`--corpus-add`/`--benchmark` local benchmark corpus: privacy-redacted per-repository snapshots, deterministic percentile/quartile placement, small-n honesty gate (< 5 peers never ranked), plus live-QA fixes ([#2334](https://github.com/bobmatnyc/trusty-tools/pull/2334)) ([`3c457e2`](https://github.com/bobmatnyc/trusty-tools/commit/3c457e22fb598310e9858af43aa702138a7df8fc))

### Fixed

- MCP tools default to local (gh) auth, App only when creds present (closes #1993) ([#1995](https://github.com/bobmatnyc/trusty-tools/pull/1995)) ([`e11a5de`](https://github.com/bobmatnyc/trusty-tools/commit/e11a5de4e2d49c19894e4f47ec694bdf414de5b0))
### Fixed

- MCP review path is now local-first: `review_pr`/`review_diff` authenticate with
  the developer's `gh`/PAT login (CLI auth) unless a GitHub App is explicitly
  configured, instead of always requiring `GITHUB_APP_ID`/`GITHUB_APP_PRIVATE_KEY`
  (closes #1993)
- unify grade computation so outer and embedded grade never diverge (closes #1886) ([#1891](https://github.com/bobmatnyc/trusty-tools/pull/1891)) ([`8f6eab1`](https://github.com/bobmatnyc/trusty-tools/commit/8f6eab15718c9446579b87166bbfd763d72c48da))
- aggregate token counts across map-reduce for shallow-review heuristic (closes #1885) ([#1890](https://github.com/bobmatnyc/trusty-tools/pull/1890)) ([`7b53d3f`](https://github.com/bobmatnyc/trusty-tools/commit/7b53d3fa2e4baf4578308babccdfc8caa11c86eb))
### Fixed

- recalibrate RC gate + verifier + Medium floor ([#1876](https://github.com/bobmatnyc/trusty-tools/pull/1876)) ([`2f0169e`](https://github.com/bobmatnyc/trusty-tools/commit/2f0169ed93924e1e35b06770b20663f4d28ee9c2))
- correct inner_findings_count metric + fail-open smell ([#1877](https://github.com/bobmatnyc/trusty-tools/pull/1877)) ([`b7c1978`](https://github.com/bobmatnyc/trusty-tools/commit/b7c1978863c7b99748cc9380bc7c3d17006987cb))
- serve-mode diff-fetch loads auth token correctly ([#1880](https://github.com/bobmatnyc/trusty-tools/pull/1880)) ([`f61ce55`](https://github.com/bobmatnyc/trusty-tools/commit/f61ce55d847588470a8b6e7d975a69388374921d))

### Changed

- bump trusty-review to v0.6.2 ([#1739](https://github.com/bobmatnyc/trusty-tools/pull/1739)) ([#1749](https://github.com/bobmatnyc/trusty-tools/pull/1749)) ([`7ef6107`](https://github.com/bobmatnyc/trusty-tools/commit/7ef6107ce9b28e45a4ee7998e2565d5867bdc5bf))
## [0.6.1] — 2026-06-25

### Fixed

- per-chunk BLOCK floors synthesis to REQUEST_CHANGES, fix log whitespace, observable floor telemetry (closes #1665) ([#1674](https://github.com/bobmatnyc/trusty-tools/pull/1674)) ([`734dac6`](https://github.com/bobmatnyc/trusty-tools/commit/734dac64))

## [0.6.0] — 2026-06-24

### Added

- LLM synthesis pass in map-reduce reduce stage (closes #1663) ([#1664](https://github.com/bobmatnyc/trusty-tools/pull/1664)) ([`1ade9a5`](https://github.com/bobmatnyc/trusty-tools/commit/1ade9a5d1e82e255b447fb995541a80551e854a5))

## [0.5.0] — 2026-06-24

### Added

- per-file map-reduce review for over-cap diffs (closes #1643, refs #1638, #680) ([#1657](https://github.com/bobmatnyc/trusty-tools/pull/1657)) ([`ea5c49b`](https://github.com/bobmatnyc/trusty-tools/commit/ea5c49bd))
- external linked-spec fetch for grounding (closes #1419) ([#1630](https://github.com/bobmatnyc/trusty-tools/pull/1630)) ([`f2e58c2`](https://github.com/bobmatnyc/trusty-tools/commit/f2e58c2f6d92262d0ed9b12d0ec50b9ea77d33d0))
- review-outcome capture — reactions/commits + outcome store + webhook poll (refs #1421) ([#1628](https://github.com/bobmatnyc/trusty-tools/pull/1628)) ([`d95fba9`](https://github.com/bobmatnyc/trusty-tools/commit/d95fba906e90f124cb76a3746fc90bb403ab117f))
- calibration harness + Rust false-positive prompt guardrail (closes #1422) ([#1629](https://github.com/bobmatnyc/trusty-tools/pull/1629)) ([`98d66d5`](https://github.com/bobmatnyc/trusty-tools/commit/98d66d5c412591bdcffb7761317fb2f78202c4cc))
- test-plan & AC conformance gaps as context (closes #1418) ([#1627](https://github.com/bobmatnyc/trusty-tools/pull/1627)) ([`675c5b1`](https://github.com/bobmatnyc/trusty-tools/commit/675c5b10fa17986297f7f8614bf01c12424bcb62))
- prior-PR change-history context source (closes #1423) ([#1626](https://github.com/bobmatnyc/trusty-tools/pull/1626)) ([`dd759d3`](https://github.com/bobmatnyc/trusty-tools/commit/dd759d33304d96569a79677c9ff775bd84d1f8cb))
- add Finding.source_citation + spec-grounding prompt wiring (refs #1419) ([#1625](https://github.com/bobmatnyc/trusty-tools/pull/1625)) ([`41e062e`](https://github.com/bobmatnyc/trusty-tools/commit/41e062e8f24c84ce61ef09a98ae55e7fe1df6026))

### Fixed

- prevent truncated diff from reaching reviewer (closes #1638) ([#1639](https://github.com/bobmatnyc/trusty-tools/pull/1639)) ([`48a7c54`](https://github.com/bobmatnyc/trusty-tools/commit/48a7c549d5482a27736e363a72cea49983784174))

---

## [0.4.0] — 2026-06-18

### Added — output fidelity (epic #1413)

- **Inline per-line PR comments (#1414).** Findings now post as inline review
  comments anchored to the exact file + diff line via the GitHub reviews API
  `comments[]` array, instead of being concatenated into one summary comment. A
  finding whose line is not in the PR diff (or has no line) falls back to the
  review summary body rather than failing the post. The would-be inline comments
  are carried on `ReviewResult.inline_comments` so dry-run / the MCP response
  previews exactly what live mode would post.
- **Committable `suggestion` blocks (#1415).** Findings that carry a concrete code
  fix render a one-click-appliable GitHub ```suggestion block (new
  `Finding.suggested_replacement` field). Malformed or multi-line/prose
  replacements degrade to a prose fix, never a broken block.
- **Consequence + uncertainty signaling (#1416).** Findings carry a new
  `consequence` field ("what goes wrong if unaddressed"), rendered as a
  `_Why it matters:_` line. Low-confidence findings (`confidence < 0.6`) are
  hedged ("This may be an issue: …") rather than asserted.
- **Nit-volume cap + what-not-to-flag guardrails (#1420).** At most 5 low-severity
  nits post inline; the overflow rolls up into a single "+N more minor nits
  suppressed" summary line. The reviewer prompt gains a concise "do NOT flag"
  section (linter-enforced style, speculative/hypothetical concerns, restating
  the diff, and consequence-free preferences).

### Fixed

- **Summary body no longer silently drops same-line findings (#1414).** The
  review summary previously decided which findings to render by matching each
  finding's `(file, line)` against the posted inline comments. Two distinct
  findings at the same `(file, line)` collided: one could be flagged as "inline"
  and then dropped from *both* the inline set and the summary body. The summary
  now partitions by finding identity using the authoritative inline index set
  carried on `ReviewResult.inline_finding_indices` (populated from the
  `InlinePlan`), so every non-inline finding is rendered and none is lost.
- **`github_issues` context query clamped to GitHub's 256-char limit.** An
  over-long assembled search query (`repo:… is:issue <keywords>`) caused the
  GitHub Search API to return HTTP 422 ("The search is longer than 256
  characters"), silently skipping the GitHub Issues context section. The query
  is now truncated to ≤256 chars at a word boundary (debug-logged when it
  occurs).
- **Corrected stale CLI help text.** `run`/`serve` help no longer claims reviews
  are "always dry-run, no comments posted" — they post live when posting is
  enabled (dry-run by default, controlled by `PR_INTELLIGENCE_DRY_RUN`).

### Compatibility

- New finding fields (`consequence`, `suggested_replacement`) and the
  `inline_comments` / `inline_finding_indices` / `suppressed_nits` result fields
  are all `#[serde(default)]` and remain OpenAI strict-mode compliant, so
  older/looser model responses and pre-0.4.0 review logs still parse unchanged.

## [0.3.16] — 2026-06-18

### Changed

- Reviewer prompt: praise from an automated reviewer is held to a higher bar
  than human praise — included only when clearly justified.

## [0.3.15] — 2026-06-18

### Changed

- **Reviewer prompt: praise findings are now optional and rare** (no longer
  mandatory per review); reviews lead with actionable findings. The
  principles addendum no longer requires at least one `praise:` per review —
  praise is reserved for genuinely notable engineering and is never used to
  soften criticism or pad a review.

## [0.3.10] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-review` now produces
  the `trusty-review` binary only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [0.3.8] — 2026-06-09

### Fixed

- **Verdict calibration: stop Medium findings from over-escalating the rolled-up
  verdict (#1015)** — advisory Medium findings no longer push the rolled-up
  verdict above `APPROVE`. The severity-floor escalation that converts a
  grade-derived verdict to `REQUEST_CHANGES` now only fires for High/Critical
  findings; Medium findings preserve the grade verdict. Prevents mechanical
  `REQUEST_CHANGES` on reviews where all findings are advisory Medium or lower.

---

## [0.3.6] — 2026-06-07

### Added

- **Prebuilt binary distribution via GitHub Releases** — the `trusty-review`
  binary is now published to GitHub Releases on every tagged version for
  `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`. Install without a
  Rust toolchain:
  ```
  curl -L https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-review-v0.3.6/trusty-review-aarch64-apple-darwin.tar.gz | tar xz
  ```
  or via `cargo install --git`:
  ```
  cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review --locked
  ```
- **README install-convention docs** — README updated with a prebuilt-binary
  install section and explicit `cargo install --git` instructions alongside the
  existing `cargo install trusty-review` (crates.io) path.

### Changed

- **`profile` Cargo feature gate** — the contributor-profile pipeline
  (`tga` + `rusqlite` heavy deps) is now gated behind the opt-in `profile`
  feature (still on by default). Users who only need core review without
  `git2`/`rusqlite` compilation overhead can build with:
  ```
  cargo install trusty-review --no-default-features --features http-server,mcp
  ```
- **Workspace MIT relicense** — the workspace `license` field was changed from
  `Elastic-2.0` to `MIT`; `trusty-review` inherits `license.workspace = true`
  and is now MIT-licensed.

---

## [0.3.5] — 2026-06-03

### Changed

- **Footer attribution (#734)** — the review footer now reads
  `Reviewed by Trusty-Review (\`<model>\`)` instead of `Reviewed by \`<model>\``.
  The brand name `Trusty-Review` appears in plain text; the model slug remains
  in backticks inside parentheses.  Both the grade-prefixed form
  (`Grade: B+ · 🤖 Reviewed by Trusty-Review (\`model\`) · …`) and the no-grade
  form (`🤖 Reviewed by Trusty-Review (\`model\`) · …`) are updated.
  Single source of truth remains `format_review_footer` in `pipeline/post.rs`.

---

## [0.3.4] — 2026-06-03

### Added

- **Letter-grade PR reviews (#732)** — the reviewer LLM now assigns a letter
  grade (A+ through F, 13 half-steps) to every review, and the verdict is
  derived from it per a fixed product mapping (APPROVE floor = B-):

  | Grade band           | Verdict         |
  |----------------------|-----------------|
  | A+, A, A-, B+, B, B- | APPROVE          |
  | C+, C, C-            | APPROVE*         |
  | D+, D, D-            | REQUEST_CHANGES  |
  | F                    | BLOCK            |

  The final verdict is `max(grade_verdict, severity_floor(findings))` so the
  grade never produces a verdict weaker than what the severity floor requires.

- **Grade → verdict reconciliation with verification** — after the adversarial
  verifier (Phase 2, #583) re-derives the verdict, the grade is clamped via
  `clamp_grade_to_verdict` so grade and verdict never disagree in the output.

- **Grade in structured output** — `ReviewResult` gains a `grade: Option<String>`
  field (serde: `skip_serializing_if = "Option::is_none"`); the MCP `review_pr`
  JSON output includes it alongside the verdict.

- **Grade in the review footer** — the posted PR comment footer now reads:
  `Grade: B+ · 🤖 Reviewed by \`model\` · tokens ↑N ↓M · est. $X`

- **Boundary unit tests** — `grade_b_minus_yields_approve`,
  `grade_c_plus/c_minus_yields_approve_star`,
  `grade_d_plus/d_minus_yields_request_changes`, `grade_f_yields_block`,
  plus `derive_verdict_with_grade_severity_overrides_grade_a` (confirmed High
  finding clamps a model "A" grade to F), and `body_footer_contains_grade`
  (pins the exact footer string).

---

## [0.3.3] — 2026-06-03

### Fixed

- **Adversarial verifier calibration — fixes 100% refutation rate** (#726)

  Three root causes were identified and fixed together:

  1. **Token truncation (root cause):** The verifier role's `default_max_tokens`
     was `16`, which is too small to hold a complete `{"judgment":"CONFIRMED","reason":"…"}`
     JSON object. Bedrock truncated the response mid-JSON, making it unparseable,
     which triggered a conservative refutation on every call.  Bumped to `128`.
     The module-level fallback constant `VERIFY_MAX_TOKENS` was also raised from
     `64` to `128` for consistency.

  2. **Biased burden-of-proof prompt:** The verifier system prompt contained
     "The default answer is REFUTED. When in doubt, REFUTE." — a strong default
     that compounded the truncation problem.  Replaced with a symmetric,
     evidence-weighing instruction that asks the model to decide on the actual diff
     content without defaulting to either verdict.  The truncation/hallucination
     hard rule (REFUTE anything referencing a file/line absent from the diff) is
     preserved unchanged.

  3. **Verdict collapse on non-clean refutations:** Unparseable/truncated verifier
     responses were mapped to plain `VerifyOutcome::Refuted`, indistinguishable
     from a clean model REFUTED.  `rederive_verdict` then treated all-refuted
     rounds as "escalation rested on refuted evidence" and dropped the verdict to
     `APPROVE`.  Two fixes:
     - Unparseable responses are now mapped to the distinct `TruncationRefuted`
       variant (already present in `VerifyOutcome`), separating infrastructure
       failures from clean model judgments.
     - `rederive_verdict` uses a three-way baseline rule: (a) any confirmed →
       keep `primary_verdict`; (b) at least one clean model REFUTED → drop to
       `APPROVE`; (c) all demotions were `TruncationRefuted`/`ErrorRefuted` →
       preserve `primary_verdict` (verifier infra failed, not the reviewer's
       reasoning).

  **Regression fixture:** `verify_join_handle_regression_pr720` models the
  dropped-`JoinHandle` finding from PR #720 (the true-positive that was wrongly
  refuted in the original incident).  Asserts that (a) a CONFIRMED judgment
  preserves the finding, and (b) a truncated judgment does NOT collapse the
  verdict to APPROVE.

---

## [0.3.2] — 2026-06-03

### Added

- **Model + token-usage footer on posted reviews** (#728) — every posted PR review
  comment now includes a metadata footer with the reviewer model slug, input/output
  token counts, and cost estimate (e.g., `🤖 Reviewed by us.anthropic.claude-sonnet-4-6 · tokens ↑1234 ↓567 · est. $0.01`).
  Single-source-of-truth with the `ReviewResult` struct fields; appears identically in dry-run output.

---

## [0.3.1] — 2026-06-03

### Fixed

- **Required-dependency health gating** (#722) — `review_health` now reports
  `status: degraded` only when a *required* dependency (trusty-search) is unreachable.
  Non-required dependencies (trusty-analyze) being unavailable does NOT degrade status.
  Centralized `compute_status` logic shared by HTTP + MCP paths.

---

## [0.3.0] — 2026-06-03

### Added

- **Inference reachability probe in health endpoint** (#719) — `review_health` now
  includes an `inference` field (`ok` | `unreachable` | `auth_error` | `unknown`),
  always-on with 10s TTL and 3s timeout. Covers Bedrock + OpenRouter availability.
  When inference != ok, service status is degraded.

---

## [0.2.0] — 2026-06-03

### Added

- **Map-reduce review Phase 1: config + selector** (#690) — structured
  map-reduce review pipeline with per-file configuration and selector logic
  to route files to the appropriate review strategy.

- **Map-reduce review Phase 2: per-file diff splitter** (#698) — per-file
  diff splitter that partitions large diffs into reviewable chunks, enabling
  reviews on PRs that exceed the single-pass diff cap.

- **Auto-derive search index from repo root** (#661) — the reviewer now
  auto-detects the trusty-search index ID from the repository root path,
  eliminating manual index configuration for standard installations.

- **Daemon http_addr discovery + auto-detect** (#665/#676) — the review
  daemon discovers the trusty-search HTTP address automatically via the
  port-lock file; manual `--search-url` override still supported.

- **list_indexes envelope parse + resolve_index wiring** (#672/#670) —
  MCP tool `list_indexes` correctly parses the response envelope and
  `resolve_index` is wired into the review pipeline.

- **GitHub Issues query cap 256 chars** (#675) — GitHub Issues search
  queries are now capped at 256 characters to stay within API limits.

- **BrokenPipe on stdin treated non-fatal** (#702) — a broken pipe on
  the MCP stdio transport is treated as a clean client disconnect rather
  than a crash, improving robustness in CI and pipe-based invocations.

- **redb 4.x dedup store recovery** (#702) — the dedup claim store is
  upgraded to redb 4.x with graceful incompatible-file recovery: existing
  redb 2.x `dedup.redb` is backed up and recreated automatically.

> **OPERATOR NOTE:** Existing `dedup.redb` is backed up to
> `dedup.redb.v2-incompatible` and recreated on first start after upgrade.
> The dedup store only tracks posted reviews (SHA-keyed claim locks), not
> review history or results — no review content is lost.

---

## [0.1.0] — 2026-05-28

### Added

- Initial release: LLM-backed PR-review service with GitHub App auth,
  MCP stdio JSON-RPC service, diff analysis with noise filtering,
  context orchestration, and dedup claim store.
