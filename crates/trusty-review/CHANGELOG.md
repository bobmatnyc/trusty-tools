# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.20.0] — 2026-08-19

### Added

- The technical due-diligence report renders a new section 8, "Ticketing &
  Delivery Traceability", stating how many commits reference tracked board items
  and which boards those items came from. Gaps & Caveats moves to section 9.
- The manifest's `[report]` section accepts a `ticketing` key naming the JSON
  artifact `tga audit` writes. A manifest that declares none renders the section
  as unassessed and names it under Gaps & Caveats; a run that correlated nothing
  states that in full rather than collapsing, so an empty board is never
  indistinguishable from an absent artifact.
- A ticketing artifact declaring a `schema_version` major this build does not
  read fails the run by name instead of loading. tga and trusty-review are
  installed independently, and every field of the artifact defaults, so a
  renamed count in a future tga would otherwise have rendered as a stated zero.
  A newer minor of a known major still loads.
- The technical due-diligence report gains an "Authorship & Key-Person Risk"
  section (#5453, #6004): bus factor, ownership concentration, single-author
  subsystems, and a trailing-12-month trajectory per application, plus a new
  `authorship_summary` LLM narrative slot (high-level, health-from-an-
  authorship-perspective framing) on the existing synthesis call, inheriting
  its numeric guardrail. Key-man risks render in this dedicated section, not
  scattered across Top Risks. The manifest's per-repository entries accept a
  new `authorship` key naming the JSON artifact `tga audit` writes; a
  repository whose artifact fails to load states that as a named gap for that
  repository only — never a silently absent section, never an aborted build.
  The Key Facts block's author-count and trajectory rows, left as named gaps
  by the earlier Code/Security/Performance change, now populate once
  authorship data exists. The artifact's `unresolved_authors` count — commit
  identities tga could not resolve to a canonical author — travels with the
  figures, so a reader can tell how far the concentration numbers under-merge.
- The report-synthesis system prompt now asks for a coverage-gaps note. When the
  analysed data references a repository, service, database schema, or project
  board that is not one of the assessed applications, the executive summary names
  it and recommends the operator register it and re-run. Schema and migration
  repositories are called out specifically — a finding citing a table or a
  migration with no schema repository in the assessed set is the common tell. The
  instruction is static, so a template overriding `executive_summary` cannot drop
  it, and it permits naming only what the data actually references — never a
  target the model infers probably exists.
- The technical due-diligence report gains three new sections: "Code Quality &
  Architecture" and "Security Posture" re-project complexity, LoC/tech-stack,
  and RED/AMBER lint-tool findings already loaded for other sections —
  Security Posture is the promoted, now actually-filled, successor to the old
  §6.1 Security Violations table (previously unfilled template scaffolding).
  "Performance & Scalability" states, in fixed text never touched by
  synthesis, that no performance data source exists and what an assessment
  would require.
- A "Key Facts" block ahead of the executive summary frontloads codebase
  density (LoC, file count, languages) and a merged complexity profile —
  deterministic, never LLM-touched. Author count, work-volume estimate, and
  monthly trajectory rows render as named gaps until the authorship artifact
  lands.
- The executive summary carries a deterministic jump-list linking to every
  section actually present in the rendered document — never a link to a
  section a custom template omitted.
- Two new LLM narrative slots, `code_quality_summary` and `security_summary`,
  join the existing synthesis call and inherit its numeric guardrail. A
  template may override either section's voice via
  `<!-- instruct:code_quality_summary ... -->` /
  `<!-- instruct:security_summary ... -->`, same as the existing three.
- The default narrative voice (executive summary, top risks, and the two new
  slots) is now explicitly balanced/adversarial: acquirer-side, skeptical of
  risk, evenhanded about genuine strengths, never promotional.
- The `report-technical-dd-cast.md` template variant intentionally does not
  get the new Code Quality & Architecture / Security Posture / Performance &
  Scalability sections, the Key Facts block, or the Contents jump-list yet —
  porting them to CAST's own methodology and health-factor voice is deferred
  to follow-up work tracked on #6004.

### Fixed

- The CAST template (`report-technical-dd-cast`) renders the ticketing-coverage
  section the generic template gained at 0.17.0. A CAST engagement whose
  manifest declared a valid `[report].ticketing` artifact showed no coverage
  figures and no gap line either: with no section in the template there was no
  placeholder to collapse, so the omission escaped the polish pass that exists
  to name absences. The section is now section 8 in both templates and Gaps &
  Caveats is section 9 in both.
- A multi-file diff no longer collapses into a single file. `parse_diff_files`
  started a file record only on a `diff --git ` line but bound the path on
  `+++ b/`, so a diff carrying `---`/`+++` pairs without those markers rebound
  the open record's path on every file and returned ONE record — named after the
  last file, holding every file's hunks. Stage A then reported `kept=1` and every
  map-reduce unit inherited the misattribution (#4458). The parser now also
  starts a record on a `---`/`+++` pair that arrives after the open record
  already has a path or hunks, and reads hunk bodies against the line budget
  their `@@` header declares so a diff-of-a-diff's body lines are not mistaken
  for file headers.
- Deleted files, binary files, mode-only changes, and pure renames now reach
  Stage A. Each of those shapes lacks a `+++ b/` line, so the old parser
  produced no record for it and the file vanished from the review with no error.
  A record's path now resolves from the first available of `+++ b/`,
  `rename to`, `--- a/`, and the `diff --git` header itself.
- Content the parser cannot attribute to any file is reported instead of
  dropped. `parse_diff_files_detailed` returns those runs as `UnparsedSection`
  entries alongside the files, and `DiffAnalyzer::analyze` logs their count at
  warn level and stamps `parsed` and `unparsed_sections` onto the Stage A line.
- The per-finding verifier retries a transient failure instead of writing the
  finding off on the first error. The transport errors that made this pass fail
  on nearly every finding came from the round's own fan-out — 27 of 29 findings
  in one measured review returned `Transport` while a single call to the same
  model succeeded in ~845 ms — so fabrication detection was effectively off.
  Each finding now gets up to `[verification] max_attempts` calls (default 3)
  with exponential backoff and jitter, and the fan-out width is
  `[verification] concurrency` (default 4, unchanged) instead of a hardcoded
  constant. Both read `TRUSTY_REVIEW_VERIFY_MAX_ATTEMPTS` /
  `TRUSTY_REVIEW_VERIFY_CONCURRENCY` over the config file. See #4459.
- A finding the verifier still cannot reach after its last attempt is recorded
  `unverifiable` — never confirmed, never refuted — rather than `error_refuted`,
  which read as a judgment nothing made and clamped the finding's confidence to
  0.10. `ReviewResult` carries a new `unverified_count` so a consumer can see how
  much of a review went unchecked. A round that rendered no judgment on anything
  can no longer relax the verdict the model itself reported. See #4459.
- `GET /status` no longer reports an in-flight count that never comes down. `POST /review` now holds the counter through the new `InFlightCountGuard`, whose `Drop` runs the decrement, so a client disconnect that drops the handler future mid-review releases the slot — as does a panic. The decrement saturates at zero rather than wrapping, and warns when it does so, since the guard is the counter's only writer and an already-zero read means one raise was released twice. See #5020.
- `trusty-review run` against a GitHub PR now opens the durable dedup claim store, so a re-run against the same `(owner, repo, pr, head_sha)` no longer posts a second comment. `build_deps_async` takes a `DedupNeed` and `dedup_need_for` derives it from the diff source and `allow_posting`; local diff sources still open nothing. `run_review` additionally aborts before any network call when posting is reachable and no store is present, so the combination fails closed wherever it is expressed. See #5113.
- A dedup claim another process still holds no longer reports `APPROVE`. `DedupStore::claim` now returns `ClaimOutcome::InProgressElsewhere` for a fresh in-progress record and keeps `Skipped` for a completed one, so the runner tells a review that never ran apart from one that ran and finished; the blocked run reports UNKNOWN with the reason instead of a verdict. Previously a process that died between `claim` and `complete` made every re-run of that PR report approval for up to two hours. See #5126.
- **The crate could not be built with `--no-default-features`.** Two files compiled unconditionally while depending on a gated feature: `pipeline/mapreduce/reduce.rs` imported `profile::synthesizer::jaccard_similarity`, and `tests/report_investigate.rs` imported `trusty_review::report`. Both are fixed, and `cargo check -p trusty-review --no-default-features --all-targets` is now clean ([#5466](https://github.com/bobmatnyc/trusty-tools/issues/5466))
  - this was already shipped in 0.15.0 — `cargo add trusty-review --no-default-features --features http-server,mcp` fails against the published crate, which is what `cargo-semver-checks` hit when it tried to build the 0.15.0 baseline
  - `report_investigate.rs` now carries `#![cfg(feature = "report")]`, matching its two siblings `report_e2e.rs` and `report_analyze_e2e.rs`. Under default features all three of its tests still run
- Report synthesis no longer fails on OpenRouter backends that enforce OpenAI
  strict schemas
  ([#5675](https://github.com/bobmatnyc/trusty-tools/issues/5675)). The
  `report_synthesis` schema was built by hand and never passed through
  `enforce_strict_mode`, so `findings.items` carried no
  `additionalProperties: false` and the provider rejected the call with
  `In context=('properties','findings','items'), 'additionalProperties' is
  required to be supplied and to be false`. The `repo_investigation` schema had
  the same gap and is fixed with it.
- Every response schema is now built through `ResponseSchema::new`, which
  applies `enforce_strict_mode` on construction, so a schema added later is
  strict-compliant without having to remember the call.
- Fields that strict mode makes required now state that an empty string is
  acceptable, so the model is not pushed to invent filler for a field it has
  nothing grounded to say in: `evidence`, `component`, `business_impact` and
  `cost_effort` in `report_synthesis`, and `business_impact` and `cost_effort`
  in `repo_investigation`. The numeric guardrail only rejects invented FIGURES,
  so a fabricated qualitative sentence would have reached the report unchecked.
- `repo_investigation`'s `line` is nullable rather than a bare integer, so a
  finding whose line cannot be placed says `null` instead of guessing.
- **An unparsable `--provider` value no longer swallows the
  `TRUSTY_REVIEW_PROVIDER` env layer**
  ([#5679](https://github.com/bobmatnyc/trusty-tools/issues/5679)).
  `resolve_role` parsed `cli_provider.or(env_provider)`, and `Option::or` falls
  through on absence only — so a CLI value that failed to parse still won that
  slot, the env var was never consulted, and resolution dropped straight to the
  config file or the built-in default. Running `--provider garbage` with
  `TRUSTY_REVIEW_PROVIDER=openrouter` exported reached Bedrock, not OpenRouter.
- Each precedence layer now parses on its own through `parse_provider_layer`, so
  a present-but-unparsable value logs a warning naming its source and yields to
  the next layer, keeping the documented CLI → env → config file → default chain
  intact.
- `bedrock_region_resolution` no longer fails on a machine with `AWS_REGION`
  exported ([#5706](https://github.com/bobmatnyc/trusty-tools/issues/5706)). The
  test asserted that an empty explicit region reaches `us-east-1`, which skips
  the `TRUSTY_AWS_REGION` and `AWS_REGION` tiers that `resolve_bedrock_region`
  is documented to consult first. The precedence walk moved into a pure
  `resolve_region_from(explicit, trusty_env, aws_env)` helper the test drives
  directly, so every tier is covered without reading or mutating process-wide
  env vars. `resolve_bedrock_region`'s behaviour is unchanged.
- A declared metrics JSON whose `schema_version` names a major this build does
  not read now fails the run by name instead of loading. Every field of the
  metrics schema defaults, so a renamed key rendered as a stated zero — a
  confident wrong LoC total, file count, or complexity distribution in a
  client-facing due-diligence report — while only syntactically broken JSON
  errored. A newer minor of a known major still loads, and a tag that resolves
  to no major at all is refused rather than assumed to be v0.
- A metrics JSON carrying no `schema_version` still loads, read as v0. Nothing
  in this workspace writes that artifact — `tga audit` omits the manifest's
  `metrics` key so the live `--analyze` fetch is not blocked — so the file is
  hand-authored against a field this crate documented as informational, and v0
  is the only schema it has ever had. This is where the metrics loader diverges
  from the ticketing loader, which refuses an untagged artifact because every
  producer of that one writes the tag.
- A trusty-search index whose semantic or graph lane failed over a healthy
  corpus no longer disappears from a degraded review's stamped reason.
  trusty-search #5927 narrowed `warmboot_summary.indexes_corpus_failed` to real
  corpus-open failures and moved the any-lane count to a new
  `indexes_stage_failed` key. `degraded_reason` read only the old key, so after
  that narrowing a lane failure built an empty clause list — which
  `serving_state` classifies as the benign network-mount case and reports as
  `Serving`. The wire mirror now carries `indexes_stage_failed` and reports the
  cohort it names, minus the corpus cohort so no index is counted twice, with
  its own consequence: those indexes answer with lexical results only.
- Report synthesis no longer hard-fails a due-diligence run when a provider
  ignores the forced JSON schema and returns markdown-headed free prose
  instead ([#6009](https://github.com/bobmatnyc/trusty-tools/issues/6009)).
  Live repro against `anthropic/claude-opus-4.8` via OpenRouter: `200 OK`,
  `finish_reason: "stop"`, `response_format` silently ignored, response was
  `## Executive Summary\n\n<prose>\n\n## Top Risks\n\n...`. The parser now
  recovers `executive_summary` from that shape (never `top_risks`/`findings`
  — prose is never reconstructed into structured rows); the numeric guardrail
  still verifies whatever text is recovered, so a fabricated figure is
  rejected exactly as before.
- An unparseable synthesis response is now persisted (scrubbed) next to the
  report output as `synthesis-unparseable-response.txt` so a future
  occurrence is diagnosable without spending another live provider call.
- Report synthesis no longer hard-fails a due-diligence run when a provider
  returns valid top-level JSON but drifts `top_risks` field names — a live
  capture used `risk` for `description` and `applications` for `apps`, and
  omitted `severity`/`cost` entirely. Those field names are now accepted as
  aliases, and an omitted `severity`/`cost` defaults to unset rather than
  failing the whole response; the rendered report shows `not stated in
  source data` for a defaulted severity/cost — never a fabricated band or
  figure.
- Fixed the CLASS of #6009, not just the two prior instances: three
  consecutive live calls to `anthropic/claude-opus-4.8` via OpenRouter, all
  with `response_format: json_schema`/`strict: true`, produced three
  different response shapes (markdown prose, `risk`/`applications`, and
  `risk`/`cost_effort_framing`/`affected_applications`) — `response_format`
  is best-effort for Anthropic-family models, since Anthropic's own API has
  no native strict-JSON mode. The synthesis system prompt now states the
  exact required JSON field names in the prompt TEXT itself, generated
  directly from the same schema definition the request forces via
  `response_format` (so the two can never drift apart), and the per-shape
  `#[serde(alias)]`s on `RiskRow` are replaced by a whitelist synonym table
  (`synthesize_normalize`) applied before typed deserialization: a
  recognised synonym (`risk`, `applications`, `affected_applications`,
  `cost_effort_framing`) renames onto its canonical field, and any other key
  is dropped with a recorded `synthesis: dropped unrecognized field` note —
  never guessed. The numeric guardrail still runs after normalization, so a
  fabricated figure is rejected exactly as before.
- The report's Key Facts block no longer renders "No data available" while the
  sweep holds the numbers (#6029). It read only a `--analyze` metrics file, so
  an ordinary run — which always produces a repository scan — left every
  density row empty and the polish pass collapsed the whole block; one real
  engagement showed that line above a scan that had measured 1.5M LoC. LoC,
  file count, and languages now take the same metrics-then-scan precedence the
  per-application Profile table already used. A row whose input is genuinely
  absent states which input is missing — complexity names `--analyze`, author
  count and trajectory name the tga authorship artifact, and the work estimate
  names the upstream effort metric that does not exist yet — instead of
  dropping out of the table and blanking the rows around it.
- **A synthesized narrative that rounds a measured figure is no longer dropped as
  fabricated**
  ([#6030](https://github.com/bobmatnyc/trusty-tools/issues/6030)).
  The numeric guardrail compared canonical strings, so an authorship summary
  writing a measured `top_author_share_pct` of 85.19 as "85%" produced
  `rejected (unverified figure) in authorship summary: 85` and the whole LLM
  narrative fell back to the deterministic composer. Live verification of #6037
  failed on exactly that.
- `allowed_numbers` now widens each measured figure with its conventional
  roundings — the truncation and the round-half-up form at every coarser decimal
  precision, carries included (85.19 admits 85.1, 85.2 and 85; 9.99 admits 10).
  Integer figures are not widened, so a measured 8234 still never admits 8,000,
  and a figure matching no measured value under any rounding is rejected exactly
  as before, dropping the field with its raw-response capture.
- `--analyze` no longer degrades to a scan-only report when the analyze daemon
  closes a pooled connection (#6038). The adapter issues its GETs back to back
  on one HTTP/1.1 keep-alive connection, and the daemon closing that connection
  races the next request — the diagnostics fetch that followed a slow
  `/complexity_distribution` failed with `error sending request`, and every
  metrics-driven section fell back to the scan. A transport failure that is
  neither a timeout nor a refused connect is now retried once on a fresh
  connection; a refused connect and a timeout stay terminal, so a daemon that is
  genuinely down still fails open at the same speed.
- The default analyzer URL is `http://127.0.0.1:7879` rather than
  `http://localhost:7879` (#6038). `trusty-analyze serve` binds the IPv4
  loopback only, and macOS resolves `localhost` to `::1` first, so a stock run
  could not reach a healthy daemon until the operator exported
  `PR_INTELLIGENCE_ANALYZER_URL` by hand. The env-var override is unchanged.
- Both analyze HTTP clients now build through
  `trusty_common::http_client::loopback_client_builder`, so an exported
  `HTTP_PROXY` no longer diverts a loopback fetch to a proxy (#4392).
- `--analyze` gives each trusty-analyze endpoint its own request budget instead
  of one flat 15 s. The diagnostics budget is derived from the daemon's own
  deadline ladder (`TRUSTY_DIAGNOSTICS_DEADLINE_SECS`, default 180 s, plus its
  90 s of handler/router/client headroom), so the daemon's answer — including
  the structured report it sends when its own deadline is hit — always outlasts
  the client. Measured live before the fix: diagnostics answered `200` at 142 s
  with `tools_run: ["clippy"]` while the client gave up at 15 s. The cheap
  endpoints keep the short budget. See #6041.
- A trusty-analyze endpoint that fails no longer discards the data the other
  endpoints returned. Complexity, diagnostics, and refactor suggestions are
  fetched independently; whatever answered is rendered, and each endpoint that
  did not is named under Gaps & Caveats along with why it dropped out (timed
  out, unreachable, or an unusable answer). A fetch where no endpoint answered
  still falls back to the repository scan rather than rendering empty metrics.
  See #6041.
- **The standalone authorship report now states why it has no data**
  ([#6046](https://github.com/bobmatnyc/trusty-tools/issues/6046)).
  A repository whose authorship artifact fails to load renders the section as
  `_No data available — see Gaps & Caveats._`, and since the split that section
  lives in the code-review report — so a reader holding only the authorship
  document saw the collapse line and no reason for it. The authorship-load gap
  lines now render as a `Data gaps in this assessment` block above the body.
  Gaps from other legs stay out of it, and a run whose authorship leg succeeded
  is byte-identical to before.

### Changed

- `SystemGhResolver` resolves `gh auth token` through
  `trusty_common::gh::GhCommand`
  ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)) instead of
  its own `Command::new("gh")`. The resolution order (`GITHUB_TOKEN` →
  `GH_TOKEN` → `gh auth token`) and the debug-log-and-return-`None` degrade are
  unchanged; the log line now carries the entry point's classified reason.
- **0.17.0, not 0.16.1 — the public API of the `report` module broke and the patch bump did not say so.** `ReportSection` and `ReportModel` each gained a required `ticketing` field, and `ReportError` gained `MetricsSchema`, `Ticketing` and `TicketingSchema`. Cargo's 0.x rule puts the breaking position at MINOR for a `0.y.z` crate, so this ships as 0.17.0. The `trusty-review-v0.16.1` tag was cut for that delta and is abandoned.
- `ReportError`, `ReportSection` and `ReportModel` now carry `#[non_exhaustive]`. A downstream `match` on `ReportError` needs a `_` arm and the two structs are built through `ReportModel::build` or manifest deserialization rather than a struct literal, so the next variant or field is a minor bump instead of a major one. No in-workspace caller was affected — neither trusty-analyze nor trusty-mpm reaches the `report` module.
- The three roles ask for a model tier instead of naming a pinned id: reviewer →
  analysis tier, verifier and summarizer → haiku tier. On OpenRouter the
  reviewer now resolves Opus 4.8, so the model that does the judging is an opus
  model rather than the Bedrock Sonnet 4.6 id it fell back to before.
- The haiku roles resolve the same model as before —
  `us.anthropic.claude-haiku-4-5-20251001-v1:0` on Bedrock. What changed is that
  a date-stamped constant became a tier lookup, so a future haiku move is one
  edit in `trusty-common` rather than three here.
- Bedrock's opus tiers are unmapped, so a standalone Bedrock run keeps its
  Sonnet 4.6 reviewer default. The trusty-audit path configures OpenRouter and
  gets Opus 4.8.
- `--reviewer-model`, `TRUSTY_REVIEW_*_MODEL`, and the config file's
  `[models.*].model` are unaffected. The tier is the built-in layer, below all
  three, so an explicitly-named id still wins. The provider now resolves before
  the model so the tier has a provider to key on.
- The verifier and summarizer roles request `ModelTier::Classification`, renamed from `ModelTier::Haiku` in trusty-common. Both roles resolve the same model as before; a doc comment at the role mapping records that they select that tier for cost, not because verifying or summarising is classification. See #5987.
- The synthesized executive summary now describes what the audited codebase is
  and does, and analyzes its major components, before covering risk (#6030) —
  it was risk-only. The instruction is static in the synthesis system prompt,
  like #5886's coverage-gaps note, so a template that overrides the
  `executive_summary` section instruction cannot drop it; the forced-output
  schema states the same requirement. The synthesis digest now also carries
  each application's repository-scan profile — LoC, languages, file count, and
  the detected build manifests with their declared project names and top
  dependencies — so the component analysis is grounded in real data rather
  than invented. Before this, a run without `--analyze` sent the model "No
  metrics available for this application" while the scan held all of it.
- `report` now renders two documents per run instead of one (#6046): the
  code-review report keeps every section it had except Authorship & Key-Person
  Risk, which becomes its own `<stem>-authorship.md` beside it, with its own
  title, provenance legend, and generation date. Both come from one fill pass,
  so the two cannot drift in wording or provenance tagging. The code-review
  report's Contents jump list no longer links the authorship section, since it
  no longer carries it. A run with no authorship data still writes the second
  document, carrying the same no-data line the section used to show inline.
  Both paths are printed to stdout as before, so `tga audit` picks them up and
  `trusty-audit` packages both without change.

### Removed

- **Contributor profiling is gone from trusty-review — it lives in `tga profile` (tga 2.19.0) now.** This removes `src/profile/` (20 files), the `profile` CLI subcommand and its `cli_profile.rs`, the default-on `profile` Cargo feature, and the `tga` + `rusqlite` dependencies that feature carried. Breaking, hence the MINOR bump to 0.16.0 (closes [#5466](https://github.com/bobmatnyc/trusty-tools/issues/5466), part of [#5468](https://github.com/bobmatnyc/trusty-tools/issues/5468))
  - the Cargo edge ran backwards: trusty-review declared `tga`, while at runtime `tga audit` shells out to `trusty-review`. Only one direction survives now, and it is not a manifest edge — `tga` resolves `trusty-review` from PATH ([#5236](https://github.com/bobmatnyc/trusty-tools/issues/5236), DOC-67 §6)
  - `trusty_review::profile::synthesizer::jaccard_similarity` was public and is removed. Its one in-crate caller, the map-reduce `dedup_findings`, keeps it as a private helper in `pipeline/mapreduce/reduce.rs` with both its tests; behaviour is byte-identical
  - dropping `profile` from the default feature set also drops a vendored libgit2 and a bundled SQLite from every default build of this crate and of anything depending on it

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- **Module docs render once instead of twice.** `config::resolve_index_tests` carried both an outer `///` on its `mod` declaration and its own inner `//!`; rustdoc concatenates the two, so the module page showed the split rationale twice. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
- Two doc links no longer fail `cargo doc` under
  `-D rustdoc::broken_intra_doc_links`. `contents_links` linked
  `super::polish::collapse_recursive`, a private `fn` that rustdoc cannot resolve
  from a public doc — now plain backticks. `Synthesizer::new` linked a bare
  `with_raw_capture_dir`, which does not resolve because a method is not in scope
  by its own name — now `Self::with_raw_capture_dir`, matching how the same
  method is already referenced elsewhere in the file.

## [0.15.0] — 2026-08-12

### Breaking

- `report` now always synthesizes, and a synthesis that produces no verified prose fails the run instead of writing a deterministic-only report (#5454).
- `--synthesize` is accepted but ignored, and prints a deprecation line — the flag stays parseable so scripts and older `tga` invocations keep working.
- `report` checks `OPENROUTER_API_KEY` before reading the manifest, so a missing credential costs nothing but the error.
- `Synthesizer::synthesize` returns `Result<Synthesis, SynthesisError>`; `SynthesisStatus`, `Synthesis::unavailable`, and `Synthesis::is_available` are removed, so the type can no longer represent a failed pass.
- `Reporter::write` rejects a model carrying no synthesis with the new `ReportError::SynthesisRequired`.
- The deterministic Executive Summary composition from #5374 is kept: it now fills §2 when the numeric guardrail rejects the model's summary.

## [0.14.1] — 2026-08-11

### Fixed

- Inline PR comments now lead with a verification caveat when the finding's own
  `verified` outcome owes one, so a refuted finding no longer reads exactly like
  a surviving one (#5312). A clean `refuted` says it was disproved and is not a
  merge blocker; `error_refuted` / `truncation_refuted` say the verifier was
  never reached and the claim is unverified, not disproved.

### Security

- DD-report findings are now scrubbed of this process's credentials before they
  reach the report (#5323). trusty-analyze's daemon output, a manifest-declared
  metrics JSON, and the investigation's verified findings previously travelled
  verbatim into the rendered finding bands, the executive summary, the synthesis
  digest sent to the LLM provider, and the JSON twin — tga's report path has had
  this guarantee since #5239 and trusty-review's had none.
- An investigation finding reaches the page by two independent routes and both
  now scrub. `apply_investigation` covers the metrics route and the investigation
  record stored on the model; `merge_investigation_prose` covers the synthesis
  route, which `FindingRow::merge_prose` overwrites the metrics prose with. The
  verbatim `evidence_quote` has no metrics route at all and is only ever scrubbed
  on the second.
- The scrub runs where findings enter each sink, ahead of every downstream
  truncation, and covers every producer-supplied string — including
  `AnalyzeMetrics.schema_version`, which a declared metrics JSON authors freely.
  Needles come from `trusty_common::credentials`' registry walk, so no secret is
  passed across a process boundary.

## [0.14.0] — 2026-08-10

### Fixed

- The technical-DD report's §2 Executive Summary no longer renders
  `_No data available — see Gaps & Caveats._` on a run without `--synthesize`
  (issue #5318). It was filled only from LLM synthesis prose, so every
  `tga audit` report collapsed the first section a diligence reader opens while
  listing real RED/AMBER findings in §5. §2 and its Top Risks table now roll up
  from the report's own data — applications, size, language mix, severity counts
  by dimension, and the application risk concentrates in — with the provenance of
  the figures used. Verified synthesis prose still wins when `--synthesize` runs.
  - When nothing measurable was supplied, §2 now names the specific missing
    inputs (no `metrics` file, no `--analyze` fetch, no scannable checkout)
    instead of collapsing to the generic Gaps & Caveats pointer.
  - The Top Risks table caps at five rows and now says so in the table itself
    ("**Top 5 of 7** — 2 further RED/AMBER finding(s) are not listed here…"), so
    a reader who skims the table without the paragraph above it cannot mistake
    five rows for the whole risk picture.
- The report's `Data gaps:` line now states how many gaps it lists and then lists
  exactly that many. It used to comma-join the labels straight after the colon, so
  a label carrying its template section number rendered as
  `Data gaps: 2. Executive Summary, …` — read as a count of two ahead of sixteen
  names. The count is now the length of the same slice the line joins, and items
  are separated with `;` so a label's own comma or leading section number cannot be
  read as a count or a list boundary (#5319).

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
