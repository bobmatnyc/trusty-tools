# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [3.1.0] — 2026-08-19

### Breaking

- `AgenticMode` is now `#[non_exhaustive]` and gained a variant, so external
  `match` sites need a `_ =>` arm. tga goes to 3.0.0: adding a variant to an
  exhaustive public enum is a major break under `cargo-semver-checks`, and the
  attribute keeps the next addition (#5251) a minor bump.

### Added

- JIRA and GitHub Issues now write rows into `work_items`, and Azure DevOps writes them through the same code path (#5219). `tga collect` gains one provider-agnostic pass, `collect::work_item_pipeline`, which asks each configured `PmAdapter` what references its own syntax finds in the commit corpus, batch-fetches them, and writes one `work_items` row plus a `commit_work_items` link per resolved reference. This is the first production caller of `build_adapters` — until now the trait, the factory and all four adapters were built, configured, validated at `Config::load`, and reachable only from their own tests.

  `github.ticket_regex` and `jira.ticket_regex` are live as a result. Both were validated at config load and then read by nothing, so setting either had no effect on any `tga collect` output. `pm.azure_devops.ticket_regex` stays live: `build_adapters` now forwards it to the ADO adapter, which previously ignored it in favour of a hardcoded `AB#N` pattern.

  **Upgrade note — this starts fetching without any config change.** `github.fetch_on_reference` and `jira.fetch_on_reference` are new and default to `true`, matching `linear.fetch_on_reference` and `pm.azure_devops.fetch_on_reference`, which already did. An existing config that names a `github:` or `jira:` section begins populating `work_items` on the next `tga collect`, issuing one issue lookup per detected reference against that token's rate budget — including for a config that names GitHub only to fetch pull requests. Set the flag to `false` on a provider to opt out. Uniform defaults are deliberate: #5219 was reopened because `github.ticket_regex` validated cleanly and changed no output, and shipping a writer that defaults off reproduces exactly that.

  Every failure is recorded at the severity its blast radius earns. A provider that fetched nothing while reporting at least one error has lost its whole corpus and fails the stage, which makes `tga collect` exit non-zero; a provider that resolved some references and errored on others records an item skip and keeps the rows it did get. A reference the provider authoritatively does not have returns not-found and is not a fault.

  Linear keeps `collect::linear_pipeline`, because it writes the provider-specific `linear_issues` table in the same pass — routing only its `work_items` half through the shared pass would fetch every Linear issue twice per run. Its `linear.team_keys` pre-fetch filter has no shared equivalent yet either, though that is an implementation gap rather than a limit of `PmAdapter`: `with_ticket_regex` already shows how per-provider config threads in at construction.
- `AgenticMode::Unknown` — a fourth agentic mode for commits whose message git
  or a forge composed, so a marker the author emitted was discarded before the
  commit object existed. It separates "we read the author's message and found
  no AI marker" from "the message we hold was never the author's".
  - Detection reaches it only when no marker matched and the message shows a
    rewrite fingerprint: a git/GitHub merge summary (`Merge branch 'x'`,
    `Merge pull request #N from …`), or a `(#N)` squash subject whose body was
    replaced by synthesized trailers. A marker always wins.
  - "No marker matched" means BOTH the builtin and the operator sets (#5414)
    came up empty. An operator marker still classifies a merge-summary-shaped
    commit; the `Unknown` verdict is decided once, after both scans, so it
    never pre-empts the operator set.
  - No migration: `commits.agentic_mode` is `TEXT NOT NULL DEFAULT 'none'` with
    no CHECK constraint, so `'unknown'` was already schema-legal.
  - `agentic_pct` in `fact_weekly_engineer` keeps `unknown` in the
    `net_commits` denominator and out of both numerators, which makes it an
    explicit lower bound. `persist_weekly_engineer`'s doc states this.
  - Reading an unrecognised `agentic_mode` string back from SQLite now yields
    `Unknown` rather than `None`.
- `tga audit`'s sweep now runs the commit ↔ board-item correlation pass as a
  ninth stage, immediately after collection. It previously ran only from `tga
  tui`, so an audit synced board data and never joined it to the commits it
  walked.
- The sweep writes `ticketing.json` beside `manifest.toml` and names it in the
  manifest's `[report]` section, carrying the correlation counts and the boards
  they came from to the due-diligence report. A run whose correlation stage
  failed writes no artifact and declares no key, so the report states the
  omission under Gaps & Caveats instead of rendering an empty section.
- `tga audit` writes one authorship artifact per repository (#5453, #6004):
  ownership concentration, bus factor, single-author subsystems, and a
  trailing-12-month monthly trajectory, derived from `commits JOIN files`.
  Bot commits and merge commits are excluded from every figure. Authors are
  grouped by the collection pass's own identity resolver
  (`commits.author_id → authors.canonical_email`), so one person's aliases
  collapse to one author; a commit the resolver never linked keeps its raw
  email and is counted into the new `unresolved_authors` figure, which earns
  its own caveat naming the count. Squash-merge attribution and vendored-path
  exclusion are NOT corrected for and are caveated verbatim in the artifact
  (derivation lives in tga per #5468's ruling, never in trusty-review). The DD
  manifest's `[[repositories]]` entries gain a new, additive `authorship` path
  field mirroring the DOC-67 §8 `velocity` field precedent; a repository whose
  artifact fails to write states that as a named gap on the manifest rather
  than aborting the run.
- A repository name that matches no `commits.repository` value now states a
  named gap instead of rendering "0 authors, bus factor 0" (#5453). The
  aggregation cannot tell an unmatched name from an empty repository — both
  count zero rows — so `tga audit` probes for the name first and, when nothing
  matched, names the repository names the database actually holds.
- Longitudinal contributor profiling — the data and orchestration layer (closes #5463, part of epic #5468): new `tga::profile` module. `selector` resolves a GitHub login, display name, or email to a canonical `authors` identity and finds the org-wide database; `batch` buckets that contributor's history into quarterly/monthly/weekly/custom `PeriodBatch` windows; `diff_sampler` attaches a category-stratified sample of real diffs per period, skipping repositories with no local checkout rather than failing the run; `synthesizer` clusters findings across periods by token-set Jaccard similarity and tags each `Recurring`/`New`/`Resolved`/`Worsening`, then derives the quality trajectory from the score slope; `reporter` writes `profile.json` and a Markdown report. Ported from trusty-review's `src/profile/`, which keeps its copy until #5466 removes it — a window where profiling exists in both crates is expected.

  Contributor profiling is tga's domain (owner ruling, #5468). trusty-review currently declares `tga` as a Cargo dependency while the runtime edge already runs the other way; this port is the first of three additive steps that let #5466 delete the backwards edge.

  **Not in this port, by scope.** The LLM transport lands in #5464 on top of `trusty_common::inference`; the GitHub issue write path and the `tga profile` subcommand land in #5465. What ships here is everything on either side of a model call: `batch_reviewer` builds the per-period prompt and parses the response body (taking a `&str`, so no client type is needed), and `synthesizer` supplies `apply_deterministic_synthesis`, `apply_synthesis_json`, and `apply_fallback_narrative`. A run with no model still produces a complete profile — scores, trend tags, trajectory, and a fallback narrative that says it is one.

  **Three deliberate divergences from the trusty-review original.** (1) `profile::types::Finding` and `Effort` are tga-native DTOs, not `trusty_review::models::{Finding, Effort}` — the review type carries citation flags, verification outcome, and issue eligibility that profiling never reads, and depending on it would recreate the edge this epic reverses. (2) `PROFILE_VERSION` is `"tga-profile-0.1"`, not `"tr-profile-0.1"`: the finding shape differs, so the two artefacts are not interchangeable on the wire and must not claim to be. (3) The database-path environment variable is `TGA_DB`, not `TRUSTY_REVIEW_TGA_DB`; a blank or whitespace-only value falls through to `<data-dir>/tga/tga.db` instead of resolving to an empty path.

  **Test coverage moved with the code** — 69 tests, ported rather than rewritten. Five from the original are held back with the code they guard: the `bedrock/`/`openrouter/` model-prefix-stripping regressions (two in `batch_reviewer`, two in `synthesizer`) belong with the provider routing in #5464, and the GitHub issue request construction belongs with the write path in #5465. Seven are new, covering paths the port introduced or the original left open: `Finding` confidence clamping, `Effort` serde, the `Worsening` tag, prose-response fail-safe, unknown-trajectory rejection, Markdown pipe escaping, and blank-`TGA_DB` fallthrough.

  No existing tga public item changed shape; the module is purely additive.
- Contributor profiling can now reach a model (closes #5464, part of epic #5468). `profile::batch_reviewer` gains `build_period_request` and `PeriodReviewer`: the period prompt is assembled into a `trusty_common::inference::ChatRequest` and sent through an `InferenceAdapter`, so the transport is the workspace's shared inference stack — the same one `classify::tiers::bedrock` already uses. `PeriodReviewer::from_slug` resolves the slug through the commons `Configurator` and credential store (plus the Bedrock Converse factory under `--features bedrock`); `PeriodReviewer::with_adapter` binds an already-built adapter. The call is fail-safe: a provider failure logs, bills nothing, and costs that period's findings rather than the run.

  tga takes NO dependency on trusty-review. The two remain separate processes: `tga audit` already spawns the `trusty-review` binary and hands it a TOML manifest, and that file is the seam. Nothing in this change adds a Cargo edge, and the residual seam question #5464 left open is answered the same way — a process boundary (the `review_diff` MCP tool or the binary tga already spawns), never a manifest entry.

  Model slugs are passed through untouched. `provider_for` routes on the `bedrock/` or `openrouter/` prefix and the adapter strips it for the wire in `ProviderId::wire_model_id`, so the prefix-stripping the trusty-review original did locally is now the commons' job; `period_request_preserves_routing_prefix` pins tga out of that business. `ChatRequest` carries no structured-output field, so the findings JSON Schema is stated in the system turn instead — the direct-JSON path in `parse_period_findings` stays the expected outcome rather than the lucky one.
- `GitHubClient` gains issue WRITE methods (part of #5465, epic #5468): `search_issues`, `create_issue`, `create_issue_comment`, and `upsert_issue_thread`, which composes the three into find-or-create. They live in a new `collect::github::issue_writer` module rather than in `client.rs`, which was already at 289 of its 500 SLOC — this repo splits at the PR that grows the file, not in a follow-up. Every method is an inherent `impl GitHubClient`, so callers still see one client.

  `upsert_issue_thread` finds a contributor's existing thread by a marker embedded in the title and comments on it, opening a new issue only when there is none. A failed search aborts instead of falling through to create: a transient 5xx read as "no thread exists" would open a duplicate issue on every hiccup, which needs manual cleanup to undo.

  Writes reuse the client's existing `Authorization: Bearer <token>` header — the same personal access token the read path already sends. Nothing in the write path inspects the credential, so moving it onto a GitHub App installation token is a change to how the client is built.

- `GitHubClient::with_api_base` points a client at a REST root other than `https://api.github.com`, which is what lets the write tests run against a local `wiremock` server instead of github.com. The read methods take the same root, so a GitHub Enterprise host would work through one seam rather than two.
- `tga profile <contributor>` (closes #5465, part of epic #5468). One command runs the whole pipeline: identity resolution, period batches, diff sampling, per-period review through `trusty_common::inference`, cross-period synthesis, and a JSON/Markdown report. `--dry-run` produces a deterministic model-free profile; `--github-issue --github-repo <owner/repo>` publishes the report to a per-contributor GitHub issue thread. This is the tga-side equivalent of trusty-review's `profile` subcommand, which #5466 removes.

  The command is the first caller of `PeriodReviewer::review_period`, and it honours what #5464 built: a period whose provider call failed is reported as SKIPPED, never as a period with no findings. `profile::PeriodRunSummary` folds each review in, counts the two outcomes apart, and prints a coverage line; when any period was skipped the report itself carries a Coverage section naming them and stating they are not evidence of clean work. Without that, an outage across twelve quarters renders as twelve clean quarters and the trajectory covers a sample the reader cannot see is smaller.

- `profile::Synthesizer` runs the narrative pass over the same shared-inference adapter, so the profile's prose no longer needs trusty-review. A provider failure writes the deterministic fallback narrative AND returns the error, so a caller can say the narrative is a fallback rather than presenting it as the model's.

- `profile::reporter_github` publishes a rendered profile to a GitHub issue thread — `GithubIssueConfig`, `issue_title`, and `upsert_profile_issue`. The title embeds the canonical email, which is what the next run searches for, so a contributor accumulates one thread rather than one issue per run.

- `Reporter::render` exposes the Markdown the reporter writes, and `Reporter::with_coverage_note` splices the skipped-period note above the first section. The GitHub issue body is that exact text, so the file on disk and the posted comment cannot drift.
- Pull requests now carry their source branch (`head_ref`) and the issue their body declares (`body_ticket_id`), and both feed the same ticket extraction as commit subjects. Schema migration v24 adds the two columns; existing databases keep their rows and read the new columns as "no claim made".
- `correlate_commits` resolves a commit's ticket key through a stated precedence — the commit's own text, then the branch name of the pull request that carried it, then that pull request's body. A key the commit declares is never displaced by either new source.
- `CorrelationOutcome` reports `from_branch` and `from_pr_body`, and the summary line prints both even when they are zero, so a source that harvests nothing is distinguishable from one that was never consulted.
- `branch_ticket_key` and `pr_body_ticket_key` apply the #5199 rejection rules unchanged: a branch such as `fix/ADR-0029-followup` yields nothing, and no JIRA-shaped token is ever read from a pull-request body. Measured over this repository's 2376 merged pull requests, that keeps out 2501 false keys across 423 distinct prefixes and recovers a genuine issue reference for 51 commits whose subject declared none.
- Azure DevOps pull requests now persist the `sourceRefName` they already parsed, so the branch harvest works there too.
- `tga audit` writes its per-stage progress events to stderr as machine-readable
  lines when the parent process sets `TRUSTY_PROGRESS_RELAY`, so a spawning tool
  can show what the sweep is doing. Unset, nothing is emitted and the command
  behaves exactly as before; a parent that sets it against an older `tga` gets
  no events rather than a failed run
  ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- The two phases after the sweep — indexing every checkout, then rendering the
  report — announce themselves on the same bus. They had no instrumentation at
  all, so a watcher's display stopped while the process ran on for minutes

### Fixed

- **`extract_ticket_id` read a JIRA/Linear key from anywhere in the commit message (#5199)** — the pattern `\b[A-Z][A-Z0-9]*-\d+\b` matches any hyphenated uppercase token, so `ADR-0034`, `DOC-67`, `UTF-8`, `SHA-256`, `RUSTSEC-2026` and `GCC-11` in a commit body all became ticket keys, overriding the GitHub issue the subject actually named. `correlate_commits` then reported those as coverage gaps against tickets that exist on no board. A JIRA/Linear key is now read from the subject line only — the first token after an optional conventional-commit prefix and an optional `[` — and the documentation prefixes `ADR`, `DOC`, `RFC` and `SPEC` are rejected outright. Measured over this repo's own 2633 commits: JIRA-shaped extractions fall from 466 across 178 distinct keys to 21 across 17, none of them a documentation citation, and 384 commits now resolve to the GitHub issue they cite instead of a hijacked one. Subject forms `PROJ-1: x`, `PROJ-1 x`, `fix(scope): PROJ-1 x` and `[PROJ-1] x` all still extract; `AB#N` and bare `#N` handling is unchanged.
- A failed Azure DevOps work-item fetch now fails the collection stage instead of being swallowed (#5219). `collect` caught the error from the `workitemsbatch` call, logged it at warn level, and returned success, so `tga collect` exited 0 with an empty `work_items` table and no way to tell that from a run that legitimately found no `AB#` references. The failure is now recorded as a stage failure, which `tga collect` turns into a non-zero exit (#5655). Linear already did this at every one of its failure arms; ADO — the writer everyone treats as precedent — did not.
- Linear collection now writes the source-agnostic `work_items` corpus, not only its own `linear_issues` table (#5219). Each fetched issue becomes one `work_items` row under `source = 'linear'`, keyed on the Linear identifier — the same string `LinearClient::extract_issue_ids` pulls out of a commit message, so the two sides match — and every commit that mentions a fetched identifier gets a `commit_work_items` link. Before this, only Azure DevOps wrote `work_items` in production, and every `tga.db` on the owner's machine had `work_items = 0`, so commit-to-board correlation had nothing to correlate against for Linear.

  `linear_issues` is unchanged and still written. It carries Linear-specific columns (`team_key`, `priority`, `assignee`) that `work_items` has no place for, and retiring it would drop those with no consumer asking for it; the two are written in the same pass, so they cannot drift.

  The commit query moved from `SELECT message` to `SELECT sha, message`. The SHA is what the join table needs, and reading messages alone made a per-commit link impossible even once the issues were known.

  `work_items.project` is left null. Linear's project lives behind a GraphQL field this crate does not query, and DOC-70 §6 routes project resolution through trusty-common's client instead — writing the team name into that column would fill the field DOC-70 filters on with something that is not a project.

  A failed `work_items` write is recorded in the run's error list and returned as an error from the writer, never downgraded to a zero count. Both tables are written inside one transaction, so a failure leaves neither half-written.
- Tilde expansion, `${VAR}` placeholder expansion, and the GitHub-token
  validation check each take the value they used to read from the process
  environment as a parameter, so their tests no longer mutate `std::env`.
  Two of those tests both set `HOME`, and one would read the other's value:
  `alias_file_path_expansion` failed twice in 200 paired runs with
  `failed to read aliases file /home/testuser/aliases.yaml`. Behaviour and the
  public API are unchanged — `expand_path`, `AliasFile::load`,
  `database_path::resolve`, and `expand_env_var` still read the same variables
  and still return the same results.
- `GitCollector` and the DD manifest derive a repository's name through one
  function (#5453). The collector had its own copy, which disagreed with the
  manifest's whenever a configured name was blank or whitespace — and a name
  the two sides spell differently joins zero rows rather than erroring, so the
  authorship section rendered confident zeroes instead of an error.
- **`core::config` and `core::config::validator` doctests no longer fail under `cargo test -p tga --doc -- --include-ignored`** (closes [#5460](https://github.com/bobmatnyc/trusty-tools/issues/5460)). Both module-level examples were fenced ` ```ignore ` as illustrative, non-runnable snippets, but rustdoc implements the `ignore` fence as `#[ignore]` on the generated test function — `--include-ignored` forces it to compile and execute anyway, producing an `E0277` (bare `?` in a `()`-returning fn) in `validator.rs` and a `Config::load` panic against a nonexistent `config.yaml` in `mod.rs`. Both are switched to ` ```no_run `: real, correctly-typed calls against `Config::load` / `ConfigValidator::new`/`.validate()` that rustdoc still compile-checks but never executes, so a signature change is still caught while the example stays safe to force-run.
- A provider failure during a period review no longer reads as a clean period. `PeriodReviewer::review_period` returns a `PeriodReview` — the findings plus a `skipped` field carrying the provider error — instead of a bare `Vec<LongitudinalFinding>` whose only failure signal was a `warn!` log. Zero findings and an unreachable provider were the same value, so a Bedrock outage across twelve quarters would have rendered as twelve clean quarters, and the trajectory derived from that would be a trend over a silently smaller sample.

  The shape was inherited from trusty-review's original `review_period`, and it is fixed here rather than after #5465 wires the first real caller to it. `PeriodReview` is deliberately not a `Result`: no caller can `?` one period's outage into aborting the whole run, which is the property the audit depends on. It is `#[non_exhaustive]`, so the parse-failure case can join it once `ChatRequest` can enforce a schema.

- `PeriodReviewer::from_slug_with_store` resolves a slug against an explicit `KeyStore`; `from_slug` is now that call with `default_store()`. `default_store` reads the machine's real keychain, so credential resolution had no deterministic test — both arms are covered now, matching the injection `trusty_common`'s own `provider_for` tests use.
- The profile-issue thread lookup could comment on the WRONG contributor's issue. `find_thread_by_marker` matched with a bare `title.contains(marker)`, and one email address can be a substring of another — `"[dev-profile] Bob Jones <bob.jones@x.com>".contains("jones@x.com")` is `true`. Since `in:title` is a token index and the two addresses share every token, a single search plausibly returns both issues, so Jones's performance profile could be appended to Bob Jones's thread. The match is now anchored on `<marker>`, which angle brackets close because they cannot appear inside an email address, and `upsert_issue_thread` refuses a title that does not carry the anchor — an issue opened under such a title is invisible to the next run, which would then open another.

- The same lookup read only the first page of search results. `in:title alice@example.com` also matches every colleague at `example.com`, so in an org with hundreds of profiled contributors the real thread sits past page one and the upsert would conclude "no thread exists" and open a duplicate. The search now walks up to 10 pages of 100 — GitHub's whole reachable result set — and when `total_count` exceeds what the walk could read it returns the new `CollectError::GithubSearchInconclusive` rather than creating.

  Both are the same class of defect: an ambiguity resolved toward `create`. Writes land on a live tracker and re-running undoes neither a wrong comment nor a duplicate issue, so every uncertain path is now an error.
- `tga collect` now exits non-zero when a collection stage's write or fetch path failed, instead of printing the failure as a warning and exiting 0 over a partially persisted database (#5655). `tga analyze` reports the same failures after its report stage, so the reports are still written.
- Every fault a provider records now carries a severity. A stage that never persisted its data (`StageFailed`) reaches the exit code; a single skipped record (`ItemSkipped`) is reported and never fails a long sweep. The split applies to ADO, Linear, GitHub, and Bitbucket alike.
- Linear `fetch_issue` no longer reports an auth or HTTP failure as `Ok(None)`. A non-2xx response is an error carrying the status and Linear's own message, so a rejected API key is no longer indistinguishable from an absent issue (#5665).
- The Linear response body carried into that error is scrubbed of the configured API key before it is truncated, so a provider that echoes the rejected key back cannot put it in the collection summary. Scrubbing runs through `trusty_common::credentials::scrub_secrets` and precedes truncation, so a key straddling the cut cannot survive as an unmatchable fragment (#5239).
- `fetch_referenced_issues` returns `Result<Vec<LinearIssue>>` and stops at the first failure instead of returning an empty vec. A run against an invalid-but-present key now records `Linear: fetch issues failed: …` as a stage failure (#5727), so it reaches both the collection summary and the exit code rather than writing zero rows and exiting 0 with no diagnostic. This changes the public signature of `LinearClient::fetch_referenced_issues`.
- **`tga audit` now indexes each audited repository in trusty-search before rendering the report** (refs [#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). `trusty-review report --analyze` looks an index up by the checkout path's basename and fails open when the daemon does not serve it — one gap line, exit 0, and the findings table, complexity distribution and health factors all empty. Nothing in the audit path indexed anything, so an unindexed repository produced a hollow report over a clean exit. The new `audit::ensure_repositories_indexed` probes each repository with `trusty-search index-status <id>` and, on a miss, runs `trusty-search index <path> --name <id>` — the id derived from the same manifest entry the renderer reads, so the two cannot disagree. The binary resolves from `TRUSTY_SEARCH_BIN`, else `trusty-search` on PATH.
- **A repository that will not index is named in Gaps & Caveats instead of silently degrading** (refs [#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). Per DOC-67 §9 the failure excludes that repository and the run continues: `audit::index_gap_lines` adds one line per distinct cause, naming every affected application, and the audit's exit status is unchanged. Reasons are scrubbed of configured credentials before they are excerpted.
- `tga audit` now starts the `trusty-analyze` daemon its report is built from, and refuses the run when it cannot. Nothing started it before: the command always renders with `trusty-review report --analyze`, DOC-67 §8 sources the findings table, the complexity distribution and the health factors from that daemon and nowhere else, and the renderer's client only ever issues GETs. On a machine with no daemon running — the ordinary case for an unattended sweep — every audit produced a report with those three sections empty and exited 0, with only a gap line inside the artifact to show it.

  The new preflight (`audit::ensure_analyze_daemon`) sits beside the two already there, the inference credential and the renderer version, for the same reason: DOC-67 §2 gives the sweep one non-interactive shot, and this precondition is knowable in milliseconds but was being discovered after minutes of collection. It probes `/health`, spawns `<binary> serve --port <port>` detached on a miss, and polls until ready, all through `trusty_common::daemon_guard`. Neither failure arm is fail-open — a spawn that fails and a daemon that never answers both stop the run, with a message naming `trusty-search`, which `trusty-analyze serve` exits on when unreachable.

  The binary is `TRUSTY_ANALYZE_BIN` else PATH, which is what makes `trusty-audit`'s existing export of that variable load-bearing on this path for the first time (#5663 set it believing it already was). The address is `PR_INTELLIGENCE_ANALYZER_URL` else `http://localhost:7879` — the same variable `trusty-review` reads, so the daemon started and the daemon queried are the same process by construction.

  The daemon is necessary, not sufficient: a repository still has to be indexed in trusty-search for the fetch to return metrics, and nothing in the audit path indexes anything. That remains open.
- `tga audit` now starts the `trusty-search` daemon before its sweep, ahead of the
  `trusty-analyze` preflight. `trusty-analyze serve` exits at its own trusty-search
  check, so on a machine with no search daemon the audit refused outright and the
  operator had to run `trusty-search start` by hand.
- A stale `trusty-analyze` that had been up for days on top of a dead
  trusty-search also refused every run: it answers `503 degraded` while the search
  daemon is unreachable, which the readiness probe reads as no daemon at all. The
  search guard running first recovers it.
- The daemon address comes from `trusty_common`'s shared `DaemonAddrLayout`
  resolver rather than a hard-coded `127.0.0.1:7878`, so an auto-ported or
  `TRUSTY_DATA_DIR`-isolated instance is found. The binary honours
  `TRUSTY_SEARCH_BIN`, else PATH.
- **The Linear API key no longer reaches operator-visible output through `LinearClient` (#5733).** Two paths carried it. `LinearClient` derived `Debug` while holding `api_key: String`, so any `{:?}` of the client — a `tracing` field, an `anyhow` context, a panic message — printed the live key verbatim; nothing formatted the client at the time, so the exposure lived in the type rather than in a log anyone had written. `Debug` is now implemented by hand: it renders `endpoint` and replaces the key with a fixed `<redacted>` marker. The mask is unconditional — no prefix, no length, nothing derived from the value — because `LinearConfig::api_key` is an unvalidated `Option<String>`: a fingerprint that echoed a fixed-length head would be safe only for `lin_`-prefixed keys and would disclose real entropy for anything else. Separately, `fetch_issue`'s 200-with-GraphQL-errors branch logged Linear's raw `errors` array at warn level, so a provider that quoted the submitted key back put it on stderr on every such response — a routine path, not a rare one. That array now goes through the same `redacted_body_excerpt` the non-2xx body already used, which scrubs before truncating. The branch's `Ok(None)` return is unchanged: Linear reports entity-not-found this way, and the logging was the defect, not the control flow.
- Seven config credentials no longer print in `Debug` output. `LinearConfig.api_key`, `GithubConfig.token`, `JiraConfig.token`, `BitbucketConfig.app_password` and `.token`, `AzureDevOpsConfig.pat`, and `ClassificationConfig.openrouter_api_key` were all rendered in the clear by a derived `Debug`, so any `{:?}` of a config section put the live value into a tracing field, an `anyhow` context, a `dbg!`, or a panic message. The OpenRouter key reached furthest: `Config` embeds `ClassificationConfig` and derives `Debug`, and `ClassificationEngineConfig` (`classify::classifier`) holds a clone of the same key, so both are fixed here. Each struct now has a hand-written `Debug` in `core::config::credential_debug` that renders every non-credential field unchanged and replaces the credential with a fixed `<redacted>` marker. The mask carries nothing from the value: none of these fields is format-validated, so a fingerprint that echoed a fixed-length head would disclose real entropy (#5733). An unset `Option` still renders as `None` — whether a credential is configured is the question a reader debug-formats a config to answer. Every impl destructures `Self` exhaustively with no `..`, so a field added to any of these structs fails compilation (`E0027`) until the impl names it, rather than silently vanishing from the output. The module-private `BbAuth` enum (`collect::bitbucket::client`) carried the same Bitbucket credential one layer down and derived `Debug` over both variants; nothing formats it, so the derive is dropped rather than replaced.
- `GitHubClient::fetch_issue` no longer treats a private repository invisible
  to the configured credential as "issue not found". GitHub answers every
  request under such a repo with 404, so a missing token or one that cannot
  see the repo used to look identical to a genuinely deleted issue and the
  caller silently collected nothing. A repo-visibility probe now
  distinguishes the two, cached once per client so it does not repeat on
  every subsequent lookup, and `fetch_issue` retries on 429/5xx the same as
  `list_issues` and `fetch_pr_commits` (#5980).
- `tga audit` defaults the analyze daemon address to `http://127.0.0.1:7879`
  rather than `http://localhost:7879` (#6038). It must match
  `trusty-review`'s default, which moved for the same reason: `trusty-analyze
  serve` binds the IPv4 loopback only, and macOS resolves `localhost` to `::1`
  first. `PR_INTELLIGENCE_ANALYZER_URL` still overrides it.

### Changed

- Azure DevOps `work_items` rows are now keyed `AB#42` rather than a bare `42` (#5219). `collect::correlate_commits` looks a commit's ticket key up against `work_items.id`, and `collect::ticket::extract_ticket_id` yields `AB#42` for an ADO reference — so no ADO row a previous run wrote could be matched by the correlation pass. The shared writer stores the adapter's canonical id, which is the form correlation searches for.

  The correlation pass was never the only writer, though: the old ADO writer inserted its own `commit_work_items` links directly, under the same bare numeric key. So an existing database holds legacy rows AND legacy links, and the next collect writes the `AB#42` rows beside them rather than replacing them. Delete the children first — `commit_work_items` references `work_items(id, source)` with no `ON DELETE CASCADE`, and every connection runs `PRAGMA foreign_keys = ON`, so removing the parent rows alone fails with `FOREIGN KEY constraint failed`:

  ```sql
  DELETE FROM commit_work_items WHERE work_item_source = 'azdo' AND work_item_id NOT LIKE 'AB#%';
  DELETE FROM work_items        WHERE source = 'azdo' AND id NOT LIKE 'AB#%';
  ```

  Both statements are safe to re-run, and neither touches a correctly-keyed `AB#` row or any other source.

  `PmTicket` gains a `project` field, carrying what ADO reports as `System.TeamProject` into `work_items.project` — the column DOC-70's board axis filters on. The struct is now `#[non_exhaustive]` so the next field is not another break. `PmSource::work_item_source` is new: it returns the database vocabulary, which is `azdo` for Azure DevOps where `PmSource::as_str` returns the display label `azure_devops`.
- `profile::ProfileError` gains an `Inference` variant wrapping `trusty_common::inference::InferenceError`, raised when `PeriodReviewer::from_slug` finds no credential or no registered factory for a slug's provider family. The enum is `#[non_exhaustive]`, so this is additive.
- tga enables trusty-common's `inference-client` feature explicitly. `config-cli` already implied it; naming it makes the profile transport's dependency deliberate rather than a side effect of the `config` subcommand staying mounted.
- `collect::errors::CollectError` gains a `GithubApi { status, endpoint, message }` variant carrying GitHub's response body. `error_for_status()` discards that body, which is where GitHub puts the only actionable part of a write failure ("Resource not accessible by personal access token") — without it a token-scope problem is indistinguishable from any other 403, and the new write path is exactly where that distinction decides what the operator has to fix. The enum is `#[non_exhaustive]`, so this is additive.
- `CollectionStats::errors` is now `Vec<CollectionFault>` rather than `Vec<String>`, and `CollectionStats::stage_failures()` returns the subset whose data is missing. `CollectionFault` renders as its message, so existing `{e}` formatting is unchanged.

### Documentation

- **`probe_args` in `audit/repo_index.rs` now names the tests that actually cover it.** Its `Test:` pointer cited `the_probe_asks_for_the_index_by_id`, a test that was never written, which broke the `Test pointers` CI gate on `main`. It now cites `the_index_invocation_names_the_path_and_the_id`, which asserts the argument vector without spawning, and `an_already_indexed_repository_is_not_reindexed`, which proves the spawned probe is `index-status <id>` and the only invocation for a served repository.
- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- **Module docs render once instead of twice.** Four modules carried both an outer `///` on their `mod x;` declaration and their own inner `//!`; rustdoc concatenates the two, so each module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - two were merged rather than deleted: `audit::real_binary_tests` (the outer alone recorded that the module is Unix-only because it drives the binary through a `#!/bin/sh` wrapper) and `collect::bitbucket::tests` (the `#[path]` resolution mechanism)

## [2.17.0] — 2026-08-12

### Changed

- `audit` now passes `--synthesize` to `trusty-review report`, so the due-diligence report carries a written executive summary, top-risk rationale, and RED/AMBER finding prose. Until now it passed only `--analyze`, and every audit report ever produced was fully deterministic (#5454).
- `audit` requires `OPENROUTER_API_KEY` and checks it before stage 1. An unset or blank key fails immediately, naming the variable and how to set it, rather than after a multi-minute sweep.
- A failed render now prints the exact command to re-run just the render; `manifest.toml` is written before the renderer is called, so nothing collected is lost.
- `audit` requires `trusty-review` 0.15.0 or newer, and checks the installed version before stage 1. The two are installed separately through PATH, and an older renderer accepts `--synthesize`, degrades to a narrative-free report whenever the model call fails, and still exits 0 — so the audit used to report a clean pass over a report with no written analysis.
- `audit` now checks the report `trusty-review` wrote, not just the child's exit status. A report carrying no written analysis fails the run and names the renderer upgrade that fixes it.

## [2.16.0] — 2026-08-11

### Added

- Agentic detection reads operator-supplied markers from a YAML file, so a target
  org's house footer is detectable without a code change or a tga release (#5414).
  The path comes from `TGA_AI_MARKERS`, defaulting to `~/.config/tga/ai-markers.yaml`;
  each entry is a `tool` label, a `mode` (`full_agentic` / `ide_assisted`), a `scope`
  (`trailer` / `message` / `email`), and a `regex` pattern.
  - New public module `collect::ai_marker_config` (`MarkerConfig`, `MarkerSpec`,
    `MarkerMode`, `MarkerScope`, `MarkerConfigError`, `marker_file_path`). Deliberately
    a separate type rather than a field on `Config`, which would have forced a major bump.
  - The shipped markers are scanned to completion first, and operator markers are
    consulted only for a commit they left unmarked. An operator marker can therefore
    classify a commit the shipped set missed, and can do nothing else to one it already
    catches — including a `full_agentic` operator entry against an `ide_assisted`
    builtin verdict, which appending alone did not prevent.
  - A marker file that cannot be read, parsed, or compiled — or that declares more than
    256 markers — is rejected whole, logged at `warn!`, and named in
    `detection_disclosure()`; the run continues on the builtin markers.
  - `detection_disclosure()` now states how many operator markers were active and where
    they came from, so an `agentic_pct` figure can be read against the run that produced it.

### Fixed

- The `openhands` trailer marker no longer classifies a human at the vendor as an
  agent. `\bopenhands\b` matched `Co-authored-by: Simon Rosenberg <simon@openhands.dev>`;
  it now keys on `openhands@all-hands.dev`, `openhands-release-bot`, or `OpenHands Bot`.
  Found by running the marker against a real `All-Hands-AI/OpenHands` clone rather than
  fixtures (#5414). Of the 51 commits the narrower pattern stops matching, 50 carry only
  a human contributor's `@openhands.dev` address; one (`06cc1ef2`) carries
  `Co-authored-by: OH <openhands@example.com>`, which is ambiguous and is left uncaught
  rather than anchored on a reserved placeholder domain.
- The `trusty-mpm` and `Claude Code` footer markers now match the markdown-link form
  (`Generated with [trusty-mpm](...)`), which 14 commits in this repo's own history use
  and the #5249 patterns missed. Catch rate on trusty-tools' history rises from
  91.03% to 91.35%.

## [2.15.0] — 2026-08-11

### Fixed

- Agentic-commit detection no longer undercounts. The seven hardcoded patterns all keyed on the literal "Claude", so a commit carrying only a house footer counted as human work — on trusty-tools' own 2434 commits the detector caught 47.7% against a 91.0% agentic share. Detection now runs a marker set with entries for the trusty-mpm footer, Devin, OpenHands and Aider alongside Claude Code, Copilot and Cursor, and also matches the author and committer email, which `tga collect` extracted and discarded. Measured on the same history the shipped set now catches 91.0% (#5249).
- `commits.ai_tool` is now populated from body footers and bot identities as well as `Co-Authored-By:` trailers, from a single scan shared with `agentic_mode` so the two columns cannot disagree. Existing databases keep their stored values until `tga backfill ai-detection-commits` is run; that repair pass reads the stored author email and classifies exactly as the forward walk does.
- `tga collect` and `tga backfill ai-detection-commits` log the active marker set and its limits once per run. Detection is marker-based, so a repository whose trailers were stripped, squashed, or rewritten reports a low share for a reason that is not "no AI assistance" — the log says so rather than leaving a reader to infer provenance from silence.

## [2.14.0] — 2026-08-10

### Fixed

- A repository `tga audit` collected from stale local refs is now named in the
  report's Gaps & Caveats section, with its remote and the fetch error, and
  states that its data may be behind the true remote state. The sweep hard-codes
  `--allow-stale`, so an unreachable remote leaves the `collect` stage reporting
  `ok`; the per-repository fetch outcomes were printed to stderr and dropped, and
  a report over months-old refs was indistinguishable from a current one (#5321,
  DOC-67 §9). `commands::collect::run_reporting_fetch` is the new entry point
  that returns those outcomes — `run` and `run_with_progress` are unchanged and
  still return `()`. The sweep's terminal table qualifies the same stage as
  `ok (N stale)`; a run where every remote was reached renders as before.
- `audit::run_full_sweep` now reports every stage on the `ProgressBus` it is
  given, instead of discarding the parameter. Each of the eight stages emits a
  start event (naming the stage and its position in the run) and a
  completed/failed event under the new `Stage::Audit`, and the collection stage
  is handed that same bus so its per-repository events land there too. A `None`
  or disabled bus stays a no-op.

## [2.13.0] — 2026-08-10

### Added

- `tga audit` now produces the report, not just the data. After the sweep it writes a `manifest.toml` into the output directory and invokes `trusty-review report --manifest <path> --analyze --out <dir>`, then prints the rendered artifact paths (#5236, #5238, DOC-67 §6).
- `tga::report::dd_manifest::build_dd_manifest` — the DD-manifest adapter, a pure function mapping the resolved config onto trusty-review's manifest schema per DOC-67 §6's field table. Writing the file is the caller's job, so the mapping, its determinism (the same config yields byte-identical TOML), and its credential handling are all unit-testable. Every string it emits is scrubbed of the credentials the config holds, so a token that reached a repository name, a report title, or a stage's error message cannot reach the artifact.
- A collection stage that did not complete is now named in the report's Gaps & Caveats section instead of only on stderr, so a missing dimension reads as unassessed rather than clean (#5239, DOC-67 §9).
- The report carries a placeholder data-handling statement recording that a formal data-retention attestation is pending (#5244, #5218, DOC-67 §10).
- A missing `trusty-review` binary is reported as a named, actionable error naming `TRUSTY_REVIEW_BIN`, the install command, and the manifest that was already written — never a panic or a silent skip. `TRUSTY_REVIEW_BIN` overrides the PATH lookup.
- `tga audit` — a strictly non-interactive acquisition-diligence command taking `--org`, `--title`, `--analyst`, `--client`, `--output`, and `--weeks`. Once started it never prompts, confirms, or waits for input (#5235, DOC-67 §2).
- `tga::audit::run_full_sweep` — a library entry point that drives the eight data-collection subcommands (collect, classify, jira sync, deployments, incidents, dora, pr-metrics, report) end to end with no TTY and no clap. It returns per-stage outcomes, so a failed stage is named rather than aborting the run or reading as a clean pass (#5217, DOC-67 §9). `tga audit` calls it instead of re-sequencing the subcommands itself (#5237).
- `tga::commands` is now part of the library rather than private to the binary, so the sweep reuses each subcommand's existing `run` function instead of a second copy of its logic.

### Fixed

- Three dangling `Test:` doc-comment pointers in `src/commands/audit.rs` are corrected (#5303). `audit_command_needs_no_positional_arguments` never matched a real test; it's repointed at the actual `audit_takes_no_positional_arguments`. `audit_command_reports_each_stage`, cited by both `run` and `print_stage_report` for two different claims, never had a matching test at all: `run`'s "exit `Ok` despite a failed stage" claim is repointed at `sweep_runs_every_stage_in_order_and_survives_failures`, the sweep-layer test that actually establishes it; `print_stage_report`'s own per-stage stdout/stderr formatting claim gets a real test, `audit_command_reports_each_stage`, backed by splitting the function into a writer-parameterised `write_stage_report` so the output is observable without capturing process file descriptors.
- `tga deployments collect` no longer fails with `unknown deployment_source ''` when `config.yaml` has no `dora:` block (#5304). `DoraConfig`'s derived `Default` zeroed `deployment_source`, and the `#[serde(default = "…")]` fallbacks only fire when a `dora:` mapping is actually present — so the `unwrap_or_default()` path bypassed them. `Default` is now hand-written through the same `default_*` functions, and an audit of a repo set without DORA configured no longer reports a failed stage.

### Changed

- The `trusty-review` binary AUDIT invokes is resolved by a pure function over the `TRUSTY_REVIEW_BIN` value, and `run_review_report` reads the environment exactly once at its entry point. Behaviour is unchanged — an override wins unless it is empty, otherwise `trusty-review` is looked up on PATH — but the resolution rule and the spawn path are now testable without mutating the process environment, which `std::env::set_var` cannot do soundly under a parallel test harness.

### Security

- `tga audit` now redacts a failed stage's error text before excerpting it, so a credential that spans the 160-character excerpt boundary can no longer survive as a prefix fragment in the generated `manifest.toml` or the due-diligence report handed to a third party. `sweep_gap_lines` takes the credential set as a required argument, and `report::dd_manifest::configured_secrets` is public so the sweep and the manifest builder redact against the same values.

## [2.12.0] — 2026-08-10

### Breaking

- MINOR, not patch. `Config` gained a public field and its fields are all public
  with no `#[non_exhaustive]`, and the new default flips identity-resolution
  behaviour for every existing `aliases_file` deployment on upgrade.

### Added

- `fuzzy_identity_fallback` config key (#4251). Unset (default) disables the
  Tier-3/4 fuzzy fallback only when a declared `aliases_file` successfully
  loaded a non-empty alias table; `true` forces it on, `false` forces it off.
  A declared `aliases_file` that fails to load, or resolves empty, leaves the
  fallback ON and logs at `error!` rather than silently fragmenting every
  identity. `IdentityResolver` gained the matching `with_fuzzy_fallback()`
  builder and `fuzzy_fallback()` accessor for callers that construct a
  resolver outside `from_config`.
- The behaviour flip is announced once at `info!` when it takes effect.
- `tga tui` — an interactive terminal view over collection and correlation
  (#5197). Three panes: a repo picker over the existing config surface
  (`repositories[]` entries are selectable; `github.orgs` entries are listed as
  discovery sources), live per-repository progress while a run is in flight, and
  a correlation results view showing which commits linked to which board items
  and which did not. Built on ratatui 0.29 / crossterm 0.28 — the workspace
  versions, matching `trusty-common`'s `monitor-tui` feature — so there is one
  ratatui major in the build graph. Raw mode and the alternate screen are
  restored on normal exit, on error, and on panic.
- Zero inference is the default and is enforced: every view, the correlation
  pass, and the pull all run with no model configured and no API key present.
  `[c]` (and `tga tui --correlate-only`) runs the deterministic
  commit-to-board-item link pass with no network at all.
- `tga::core::progress` — an optional, non-blocking progress bus. Pipelines
  publish `ProgressEvent`s carrying stage, target, counters, and a terminal
  outcome; delivery is bounded and drop-oldest, so a slow or absent consumer can
  never stall or fail a pipeline. The bus is opt-in at every emit site
  (`ProgressBus::disabled()` is what all existing CLI paths pass and makes every
  emit a no-op), so CLI behaviour including the `indicatif` bars is unchanged.
  `ProgressAggregate` folds the event stream into renderable rows.
- `tga::collect::correlate_commits` — the deterministic commit ↔ board-item
  link pass, plus `tga::core::db::correlation` read views (`correlation_counts`,
  `correlation_rows`, `CorrelationFilter`). The pass links a commit only when
  its ticket key matches a `work_items` row that is already present; a key with
  no matching item is reported as a gap, never invented. Idempotent. The
  `work_items` / `commit_work_items` schema is unchanged.
- `CollectionPipeline::with_progress` attaches a bus to the per-repository
  collect loop. Every configured repository reaches a terminal event, including
  one that cannot be opened.

### Fixed

- Identity resolution no longer runs the Tier-3/4 Jaro-Winkler fuzzy fallback
  when a comprehensive `aliases_file` is configured (#4251). As the alias
  roster grew from 130 to 177 entries, the fuzzy tiers began collapsing
  distinct people onto similarly-spelled colleagues — `Cristian Dominguez` →
  `Crislaine Tripoli`, `Ravi Chandrasekaran` → `Ravi Pandey`, `Gauri Saykar` →
  `Gaurav Sharma`, `Josh Taylor` and `Joseph Ku` → `Joshua Lepage` — none of
  which had a declared alias explaining the match. An author that matches no
  declared alias is now reported under its own raw name instead of being
  guessed. Tier-1/2 exact alias resolution and the #2253 email-domain gate are
  unchanged.
- Identity resolver tests now use `tempfile::TempDir` instead of a hand-rolled
  `process::id() + SystemTime` directory name. The old scheme was not unique
  within a test binary — `process::id()` is constant across parallel tests and
  the `SystemTime` remainder is coarser than a nanosecond — so tests collided
  and each one's cleanup deleted a directory another was still using. Because
  `Config::resolved_aliases()` swallows alias-file load errors, the victim got
  a resolver with zero members, which returns every input unchanged and so
  satisfied the #4251 pass-through assertions *vacuously*. Measured at 10
  failures per 100 runs. The affected tests now also carry an explicit
  non-vacuity assertion that the roster loaded.

---
- A repository whose weekly walks failed reported `Completed` — "ok, 1 commit" —
  to the progress bus. Per-week failures (a `collect_window` error, a
  `collection_runs` lookup or record failure) reached `stats.errors` and the
  terminal event was emitted unconditionally, so `Outcome::Failed` was
  unreachable from that path. It now reports `Failed` with the error count and
  the first message, still exactly one terminal event per repository.
- `tga tui` had no surface at all for a collection error: the run summary read
  only `commits_collected`. It now reads `collected N commit(s), M error(s); …`,
  which is what the `Finished` status line shows.
- Quitting `tga tui` mid-run abandoned the worker silently — it is a detached
  thread nothing joins, so `q` / `Esc` / `Ctrl-C` cut an in-flight fetch or
  per-week write off at process exit with status 0. A quit during a run now
  takes a confirming second press, and says so.
- The pre-walk progress event tagged position-among-repositories onto the
  repository's own row, so a large repo mid-walk displayed "4/5" as if it were
  80% through itself. Nothing emits intra-repo progress, so the row is now
  "in flight, size unknown" until its terminal event; the how-many-repos
  roll-up stays in the stage header.
- The first `tga tui` frame against an unmigrated database rendered corrupted:
  the migration log lines printed before the alternate screen showed through
  ratatui's diff-rendered output and persisted across redraws. The TUI now
  clears the screen before its first draw.
- `tga tui`'s screen was corrupted for the entire duration of every `[r]`
  pull+correlate, permanently — three writers reached the terminal behind
  ratatui's back, and its diff renderer never repaints a cell it believes
  unchanged, so the garbling survived every later redraw and lasted the whole
  session. All three are now suppressed or rerouted for exactly as long as the
  TUI owns the screen, and each keeps its previous behaviour on every non-TUI
  path:
  - the revwalk's `indicatif` spinner (a 100 ms steady tick for the length of
    the walk) draws to `ProgressDrawTarget::hidden()` whenever a progress bus is
    attached, via the new `GitCollector::with_progress`;
  - the collect pipeline's `println!` / `eprintln!` operator lines — the
    per-week `Collected W31 2026: …` line among them — are published to the bus
    as `Collect` detail events instead;
  - the `tracing` subscriber's writer becomes an in-memory capture for the
    `tui` subcommand only, drained once per render tick into the ACTIVITY pane,
    so warnings that used to be written over the frame are now read in it. This
    matters at the default level with no `RUST_LOG` and no `-v`: a bare
    worktree whose `origin` is not fetchable emits the fetch-failure `WARN`.
    Every other subcommand keeps the byte-identical stderr path, which is what
    keeps stdout clean for MCP framing.

### Changed

- **MSRV raised to Rust 1.94** (was 1.91). `aws-config` >= 1.9.0 and
  `aws-sdk-bedrockruntime` >= 1.136.0, published 2026-07-08, declare
  `rust-version = "1.94.1"`; because those are unpinned caret ranges in the
  workspace manifest, `cargo install` **without `--locked`** re-resolves into
  them and then refuses to build on rustc below 1.94.1 — the reported
  `cargo install trusty-code` failure on rustc 1.91.1. Users on rustc
  1.91-1.93 must `rustup update` before installing any `trusty-*` crate. See
  [ADR-0029](../../docs/adr/0029-msrv-1-94-and-edition-policy.md)
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))
- `indicatif` now resolves through `[workspace.dependencies]` instead of a
  duplicate local `0.17` pin.

## [2.10.0] — 2026-07-27

MINOR, not patch. The JIRA ingestion work (#3966, #4084) landed on `main`
without a version bump while 2.9.4 was already live on crates.io, so the
version-parity guard (#3366) went red: 11 files added and 9 modified under a
published version number. The drift is purely additive but it is *public API*,
not internals — `tga` auto-detects `src/lib.rs` as a library target, so these
are real SemVer-relevant additions:

- `collect::errors::CollectError` gained `Throttled`, `IncompleteChangelog`
  and `PagingBudgetExceeded`, and became `#[non_exhaustive]`.
- `collect::jira` gained public modules `http`, `jql_time`, `model`, `paging`,
  `retry`, `sync`, and re-exports `ChangelogIssue`, `ChangelogWalk`,
  `JiraComment`, `JiraTransition`, `RetryPolicy`.
- `core::db` gained the public `jira_facts` module and its re-exports.
- `commands` gained the public `jira` module; `commands::args` gained
  `JiraSubcommandArgs` / `JiraSubcommand`.
- `core::config::JiraConfig` gained a `timezone` field.

Nothing was removed and no existing public signature changed, so this is not a
major bump. The one published consumer, `trusty-review` (optional `profile`
feature), only constructs `CollectError` through `#[from]` and never matches on
it exhaustively, so `#[non_exhaustive]` costs it nothing.

No crates are published by this change.

### Added

- JIRA changelog + comment extraction client, covering issue transition history and per-comment detail (closes [#3966](https://github.com/bobmatnyc/trusty-tools/issues/3966)).
- `fact_ticket_transitions` + `fact_jira_comment_detail` schema, introduced by migration `0023_jira_ingestion`.
- `tga jira sync` and `tga jira freshness` subcommands to drive ingestion and report per-project data freshness.
- `tga jira freshness --project <KEY>` scopes the freshness guard to one project; with no flag it now checks every project carrying a sync cursor individually, so one project's ongoing writes can no longer mask another project's dead sync.
- Bounded retry with exponential backoff (429 / 5xx / timeouts) on the JIRA paged read paths, so a transient rate-limit response during a backfill no longer turns into a ticket-level ingestion failure.

- `jira.timezone` config key pins the IANA timezone in which the JIRA account evaluates JQL date literals; when unset it is discovered from `GET /rest/api/3/myself` and cached.
- `tga jira freshness --max-cursor-lag-days N` fails when a project's sync cursor falls that far behind, catching a sync that runs on schedule but never catches up. Cursor lag is always reported; only this flag makes it fail.

### Changed

- `CollectError` and `ChangelogIssue` are now `#[non_exhaustive]`. Both grow with every provider failure mode or per-ticket verdict the collector learns — `CollectError` gained `Throttled`, `IncompleteChangelog` and `PagingBudgetExceeded` across #3966 and #4084, and `ChangelogIssue` gained `truncated_history_total` — and on a published crate each such addition was a SemVer-major break, forcing a minor bump for what is genuinely a patch. Downstream crates must now carry a wildcard `match` arm on `CollectError`; `ChangelogIssue` was already receive-only outside the crate (its sole constructor is `pub(crate)`), so that half costs callers nothing. Every in-tree `match` already used a wildcard, so no call site changed.

### Fixed

- JIRA changelog pagination no longer silently truncates a ticket's oldest status transitions. The search-embedded changelog is itself paged, so `search_with_changelog` now compares JIRA's `changelog.total` against the embedded entry count and flags the shortfall; `tga jira sync` then walks the dedicated `GET /issue/{key}/changelog` endpoint to exhaustion and replaces the truncated history. A repair that cannot retrieve every entry the server reported fails with the new `CollectError::IncompleteChangelog`, naming the ticket and the expected-vs-retrieved counts, rather than returning a partial history that reads as complete — and the knowingly-short embedded copy is not persisted either. Unblocks the #3966 historical backfill (closes [#4084](https://github.com/bobmatnyc/trusty-tools/issues/4084)).
- A ticket whose changelog repair fails no longer costs every other ticket its data. The repair originally ran inside the search walk, which `run_sync` awaits in full before writing anything, so one `IncompleteChangelog`/500/403 meant zero rows for all 10,000 tickets, no cursor movement, and a deterministic repeat on the next run. It now runs in the per-ticket loop, reusing the failed-ticket / cursor-clamp / circuit-breaker machinery the comment fetch already had: the bad ticket holds the cursor, the rest still land, and the run still exits non-zero naming it.
- The changelog repair no longer accepts a partial history as complete when the dedicated endpoint omits `total`. Under `#[serde(default)] u64` an absent `total` read as `0`, ending the walk after page 1 and making the `retrieved < total` completeness check pass vacuously — so the repair could replace a truncated history with something no larger. `total` is now `Option<u64>`: an unstated one ends the walk only on an empty page, and the count the search itself reported is carried in as a standing lower bound.
- `fetch_comments` no longer silently ingests a partial comment set. Termination was `start_at >= total` with `total` defaulting to `0` when absent, so a response omitting the field ended the walk after one page — observed as 100 of 150 comments ingested with no error. Because the fetch still returned `Ok`, the failed-ticket cursor clamp did not protect it: the cursor advanced past the ticket and the loss was permanent. The walk now terminates on a short page.
- `fetch_comments` measures "short page" against the page size the server APPLIED (the `maxResults` every Atlassian paged bean echoes), not the one requested. Comparing against the request re-entered the same fail-open from the other end: a server paging 50 while the client asks for 100 made page 1 look short, ingesting 50 of 150 comments and returning `Ok` so the cursor advanced. An absent `maxResults` now ends the walk only on an empty page.
- Both per-ticket JIRA paged walks are bounded (200 pages) and report `CollectError::PagingBudgetExceeded` instead of looping forever against a server that ignores `startAt`.
- The JQL date renderer's step-back search no longer emits an unvalidated bound when it exhausts its ceiling. The ceiling was 180 minutes on the stated grounds that "historic DST shifts never exceed two hours" — false, as `Antarctica/Casey` folds exactly three hours (UTC+11 → UTC+08), landing on the ceiling with zero margin and saturating the loop. Saturation is now an explicit error with a remediation hint rather than a silently-emitted literal, and the ceiling is derived from the −12:00…+14:00 UTC-offset span instead of an empirical claim about tzdata's contents.
- `tga jira sync` renders its JQL `updated >=` bound in the JIRA account's timezone rather than UTC. JQL date literals are zoneless and JIRA evaluates them in the querying account's profile timezone, so on a non-UTC account every sync window landed hours away from the instant it encoded — silently skipping tickets on negative-offset accounts and stalling the pager on positive-offset ones. The renderer is also correct across DST folds and gaps, where a local literal maps to two instants or none.
- `tga jira sync` aborts after 10 consecutive comment-fetch failures instead of walking every remaining ticket. Under sustained rate-limiting a 10,000-ticket run previously issued up to 40,000 requests and slept for hours against a server explicitly asking it to stop.
- JIRA 429/503 responses now honour `Retry-After`, under a whole-run backoff budget (120s) that bounds total time lost to throttling regardless of ticket count.
- `tga jira sync` reports when a run is incomplete without being an error: `--max-tickets` truncation and any minute that had to be walked by offset are both surfaced in the run summary.
- A clean `tga jira sync` no longer moves the stored cursor backwards, so `--since 2020-01-01` or a truncated `--backfill` cannot rewind a healthy incremental cursor. A failure clamp may still regress it, which is its purpose.
- `tga jira freshness` also checks the configured `jira.project_key` even when it has no cursor row, so a project that has never completed a sync is no longer invisible to the default check.
- `tga jira sync` no longer advances the incremental cursor past a ticket whose comment fetch failed. Previously the cursor moved to the batch maximum, leaving the failed ticket permanently below every later `updated >=` window — its comments were lost silently, with the process still exiting 0. The cursor is now clamped at or below the earliest failure, the failure count is printed in the run summary, and the run exits non-zero.
- `tga jira sync` paginates the changelog walk by re-anchoring the `updated >=` window instead of by `startAt` offset. Offset paging over `ORDER BY updated ASC` let a ticket edited mid-walk shift an unread ticket across the read boundary, permanently excluding it from later runs.
- `tga jira sync --dry-run` now fetches comments and reports their real count instead of always printing `0 comment(s)`; it still writes nothing and leaves the cursor untouched.
- A JIRA project key that is not `[A-Za-z][A-Za-z0-9_]*` is rejected locally instead of being interpolated into JQL, where it could silently widen the query and let another project's tickets advance this project's cursor.
- A `${ENV_VAR}` JIRA credential whose variable is unset is now a startup configuration error naming the field and variable, instead of an empty password surfacing as an opaque HTTP 401.

---
## [2.9.4] — 2026-07-21

### Fixed

- doc-comment `Test:` pointer citation in `collect::git::reachability` corrected to a resolving glob (`tests::glob_*`) by the workspace-wide sweep in #3588 — no functional change, but it moved this crate's published `src/` out of parity with the still-current `2.9.3` version number (issue #3366); this release re-aligns them.
- establish channels crate topology + wire Slack auth ([#2638](https://github.com/bobmatnyc/trusty-tools/pull/2638)) ([#2706](https://github.com/bobmatnyc/trusty-tools/pull/2706)) ([`9f2275b`](https://github.com/bobmatnyc/trusty-tools/commit/9f2275b6487296a08fc99de469e6b641992bc46d))
- bound external-source enrichment concurrency to fix mid-run classify stall (closes #2719) ([#2720](https://github.com/bobmatnyc/trusty-tools/pull/2720)) ([`96d0cba`](https://github.com/bobmatnyc/trusty-tools/commit/96d0cbaf6922acb3febcedf483a6866207211ed0))
- SLOC-counter /* trap + stray conflict markers (closes #2489, #2509, #2390) ([#2561](https://github.com/bobmatnyc/trusty-tools/pull/2561)) ([`e4aed64`](https://github.com/bobmatnyc/trusty-tools/commit/e4aed64cfffff0eee3593669879b0cf136188137))

### Changed

- convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))
- retire duplicate Bedrock ports onto trusty-common::inference ([#2567](https://github.com/bobmatnyc/trusty-tools/pull/2567)) ([`31c9fc0`](https://github.com/bobmatnyc/trusty-tools/commit/31c9fc0467adfda65b702c7433ceded01e0cf884))
- mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))

### Documentation

- add missing package metadata to 7 crates ([#2293](https://github.com/bobmatnyc/trusty-tools/pull/2293)) ([`ee58b6a`](https://github.com/bobmatnyc/trusty-tools/commit/ee58b6a4ae01e1338e4761aaa5c27053c49f192b))

## [2.9.2] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

### Added

- add commit_count_net to weekly reports excluding reverts ([#2248](https://github.com/bobmatnyc/trusty-tools/pull/2248)) ([`68d3872`](https://github.com/bobmatnyc/trusty-tools/commit/68d38724c66bcd945574b7031ff243298ea836ab))

### Fixed

- seed primary email + local-part Tier-3 fuzzy to prevent identity collisions/misattribution ([`b8f5291`](https://github.com/bobmatnyc/trusty-tools/commit/b8f52914e4c8f33b196029fc4e6b6d5d20d40431))
- store full Bitbucket PR commit SHA list, not abbreviated merge SHA ([#2251](https://github.com/bobmatnyc/trusty-tools/pull/2251)) ([`e57a036`](https://github.com/bobmatnyc/trusty-tools/commit/e57a036bcbf1cb149dd428191c0ca87051a4c2b0))
- seed primary_email as alias for non-aliased team members to avoid empty canonical_email collisions ([#2249](https://github.com/bobmatnyc/trusty-tools/pull/2249)) ([`4f9cef3`](https://github.com/bobmatnyc/trusty-tools/commit/4f9cef3e7141c31288c31257266a3fe145732897))
# Changelog

All notable changes to trusty-git-analytics will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.9.0] - 2026-07-07

### Added

- **`tga rules list --format json` — machine-readable taxonomy rollup (#2205)**
  — emits the subcategory→top-level taxonomy rollup (built-in defaults merged
  with any `classification.custom_categories`) as JSON, so downstream
  consumers (e.g. cto-reports) can consume `TaxonomyRegistry` programmatically
  instead of hand-copying it as a Python dict. `--format text` (the previous
  behaviour) remains the default.
- **`--source file` for `tga deployments collect` and `tga incidents collect`
  (#2204)** — generic JSON/CSV file ingestion with a documented, stable
  schema for each (a flat `fact_deployments`-mirroring record for
  deployments; a generic `{incident_id, detected_at, resolved_at, severity,
  repo, triggering_deploy, jira_ticket}` record for incidents). Lets
  downstream consumers (e.g. cto-reports) go through the supported `tga …
  collect` CLI contract instead of writing `fact_deployments` /
  `fact_incidents` directly into `tga.db` via raw `sqlite3`. See the
  `commands::deployments::file_source` and `commands::incidents::file_source`
  module docs for the exact schemas.

## [2.8.1] - 2026-06-16

### Fixed

- **`tga backfill ai-detection-commits` now also repairs `agentic_mode` (#1334)**
  — the repair path previously updated only `is_ai_assisted` and `ai_tool`,
  leaving historical commits' `agentic_mode` stuck at its `'none'` default. As a
  result, Claude Code commits backfilled after migration v21 were never
  queryable as `agentic_mode = 'full_agentic'`, and downstream consumers
  (e.g. cto-reports' agentic-% analytics) under-counted agentic activity. The
  backfill now recomputes `agentic_mode` via the same `detect_agentic_mode()`
  logic used by the forward `tga collect` INSERT path and writes it alongside
  `is_ai_assisted` / `ai_tool`. **Operators should re-run
  `tga backfill ai-detection-commits` to repair existing history.**

## [2.7.1] - 2026-06-07

### Changed

- **First public prebuilt-binary Release** — tga is now distributed via the
  generic binary release workflow (`release.yml`), publishing
  `tga-<version>-aarch64-apple-darwin.tar.gz` and
  `tga-<version>-x86_64-unknown-linux-gnu.tar.gz` (with paired `.sha256`
  checksums) to GitHub Releases on every `tga-v*` tag push.
- **Install-convention README adoption** — the crate README follows the
  workspace install-convention (`INSTALL-CONVENTION.md`) so curl-install
  instructions and Homebrew tap stubs are consistent with trusty-analyze,
  trusty-review, and trusty-search.
- **Workspace MIT relicense** — `tga` is MIT licensed (consistent with the
  workspace-level MIT default for distributable tooling crates).

## [2.6.1] - 2026-06-04

### Fixed

- **GitHub (and all other provider) clients now expand `${ENV_VAR}` references
  in config credentials (#741)** — the GitHub client was passing the literal
  `${GITHUB_TOKEN}` placeholder string as the Bearer token, causing GitHub to
  return 401 Unauthorized and silently drop all PR collection. The Linear
  client had a private `expand_env_var` helper, but it was not shared. A new
  `collect::env_expand` module provides the canonical implementation; the
  GitHub, Bitbucket, Azure DevOps, and Linear clients all now run their
  token/key/PAT through it before use so `${ENV_VAR}` placeholders in
  `config.yaml` work consistently across every collector.

## [2.2.1] - 2026-05-29

### Fixed

- **`tga classify` no longer hangs ~120s at exit with external sources enabled
  (#397, bug 1)** — `tga` used the default multi-threaded Tokio runtime via
  `#[tokio::main]`. On return the runtime drop blocks until every background
  task finishes, including `reqwest`'s keep-alive connection-pool reaper, whose
  idle sockets against a live server (e.g. JIRA/Atlassian) linger up to the
  ~90s pool idle timeout. The process therefore completed all work, printed its
  summary, then hung before terminating. `main` now builds the runtime
  explicitly and calls `Runtime::shutdown_timeout(0)` after the async body
  completes, dropping idle connection tasks immediately so the process exits
  promptly. All real work is awaited before shutdown, so nothing is discarded.

- **`tga classify` no longer fails with "database is locked" immediately after
  `tga collect` (#397, bug 3)** — connections were opened without a
  `busy_timeout`, so a `classify` that opened while a just-finished `collect`
  was still flushing/checkpointing its WAL hit `SQLITE_BUSY` and failed
  instantly. Every connection now sets `PRAGMA busy_timeout = 5000`, so a
  transient checkpoint lock is waited out (up to 5s) instead of erroring.

- **`classifications.complexity` (1–5) is now populatable via a discoverable
  command (#397, bug 2)** — the complexity scoring logic shipped in 2.2.0
  (the LLM tier produces scores; the pipeline persists them) but was only
  reachable via the non-obvious `tga classify --backfill-complexity` flag. The
  normal `tga classify` run only consults the LLM for low-confidence commits,
  so rule-/JIRA-resolved commits keep `complexity = NULL`. A new
  `tga backfill complexity` subcommand exposes the existing
  `ClassificationPipeline::backfill_complexity` operation where operators look
  for it. It fills every `complexity IS NULL` (non-`exact_rule`) row via the
  LLM, supports `--dry-run` and `--use-llm`, and leaves
  category/confidence/method untouched. (No new scoring feature was built; this
  is a wiring/discoverability fix.)

## [2.2.0] - 2026-05-29

### Added

- **Developer Quality metric (#377)** — weekly reports now include a per-developer
  Quality score (1–5 scale) alongside existing velocity metrics. New columns added
  to weekly reporting output:

  - `revert_count` — number of revert commits authored in the reporting window
  - `bugfix_count` — number of commits classified as bugfix
  - `ticketed_count` — number of commits that reference a ticket ID
  - `quality_score` — composite numeric score (1–5) derived from the above signals
  - `quality_tshirt` — human-readable t-shirt size label (`A`–`E` mapped from score)
  - `abandoned_pr_count` — pull requests opened but never merged within the window

- **Unified revert detection** — revert commits are now identified consistently
  across all report types using a single shared regex-based detector. Previously
  revert heuristics were duplicated across classify and report stages.

- **Abandoned-PR tracking** — `tga collect` now records the opened-at timestamp
  for pull requests. The weekly reporter counts PRs that have been open for more
  than a configurable threshold (default: 14 days) without merging as
  "abandoned," surfacing stale work in team quality snapshots.

## [2.1.1] - 2026-05-28

### Fixed

- **HTTPS remote fetch support** (#337). Enable `git2` `https` and `vendored-openssl`
  features so `tga collect` can fetch from `https://` remotes without an external
  system `git` pre-fetch. The always-fetch behavior shipped in 2.0.0 now works for
  the common case of GitHub/GitLab https remotes:
  - macOS: uses the native Security framework (no openssl runtime dependency)
  - Linux: vendors openssl statically (portable binaries, no system libssl)
  - Windows: uses Schannel (no openssl runtime dependency)
- The pre-2.1.1 workaround (pre-fetch with system git, then `tga collect --no-fetch`)
  is no longer necessary for https remotes.

## [2.1.0] - 2026-05-28

### Added

- **`tga author <email>` per-engineer drill-down subcommand (#325)** — produces a
  focused report for a single canonical identity covering four sections:

  - **Commit Summary** — total commits, ticket coverage fraction, repositories
    touched, first/last commit dates, total insertions/deletions.
  - **Effort Histogram** — XS/S/M/L/XL bucket counts from `fact_commit_effort`
    (populated by `tga backfill effort`), with a "N / M commits scored" coverage
    header so readers know if the histogram is incomplete.
  - **Pull Request Metrics** — total PRs, merged PRs, avg/median/p95 cycle time
    (hours). Cycle times are filtered to the [0.5, 720] hour window matching
    the existing velocity computation. p95 is omitted when fewer than 20 merged
    PRs are present in scope.
  - **Category Breakdown** — per-category commit counts (feature, bugfix,
    maintenance, etc.) from the classifications table.

  Supports `--format markdown` (default, human-readable tables) and
  `--format json` (machine-readable, suitable for CI dashboards). Both `--since`
  and `--until` (ISO8601 `YYYY-MM-DD`) are included in the MVP to scope the
  report to a specific time window.

- **`tga aliases add-login <email> <provider> <login>` subcommand (#325)** —
  appends a provider login (e.g. GitHub username) to `authors.aliases` so that
  `tga author` can match pull-request authorship across providers without a
  schema migration. Provider is validated against the allow-list
  (`github`, `gitlab`, `ado`, `bitbucket`). Duplicate logins are silently
  ignored (idempotent). This is a one-time setup operation per developer per
  provider:
  ```bash
  tga aliases add-login alice@example.com github alice-gh
  tga aliases add-login bob@example.com github bobm
  ```

### Implementation notes

- PR canonicalization uses option (a) from the spec: provider logins are stored
  in `authors.aliases` as non-email entries (no `@`). The PR query extracts
  logins from the JSON array at query time. No schema migration is required.
- Median and p95 cycle time are computed in Rust by sorting the raw duration
  vector — SQLite has no native `MEDIAN` aggregate.
- The effort histogram join path: `fact_commit_effort.sha → commits.sha →
  commits.author_id → authors.canonical_email`.
- When `tga author` is called and no PRs are found, a note in the PR Metrics
  section instructs the user to run `tga aliases add-login` to map provider
  logins.

## [2.0.0] - 2026-05-28

### BREAKING CHANGES

- **`tga collect` now walks ALL local branches and remote tracking refs by
  default** (`refs/heads/*` + `refs/remotes/origin/*`), not just HEAD ancestry
  (#331). This fixes a data-integrity bug where commits on non-default branches
  (PR branches, feature branches, hotfixes) were silently excluded — the
  HEAD-only walk was losing ~56% of commits in multi-branch repos.

### Added

- **`--head-only` flag on `tga collect`** — restores the legacy HEAD-only walk
  (≤ 1.5.4 behaviour) for all repositories when passed on the command line.
  For a per-repo opt-out, set `head_only: true` in the `repositories[].head_only`
  field of your YAML config.

- **`--branch <NAME[,NAME…]>` flag on `tga collect` (#332)** — restrict the
  revwalk to one or more branch names (comma-separated). Seeds from both
  `refs/heads/<name>` and `refs/remotes/origin/<name>`, so local and
  remote-tracking copies are both covered. Mutually exclusive with `--head-only`.
  Per-repo branch overrides in YAML (`repositories[].branch`) are unchanged and
  still take precedence over the per-repo logic, but the new `--branch` flag
  takes precedence over everything for the current CLI invocation.
  Example: `tga collect --branch main,release/1.0 --repos my-service`

- **Uniform `--repos`, `--weeks`, `--since`, `--until`, `--force` filters on
  `tga classify` (#332)** — `tga classify` now accepts the same date/repo filter
  flags used by `tga collect`, enabling surgical re-classification of a specific
  slice without touching the full database:
  ```bash
  tga classify --force --repos my-service --weeks 4
  tga classify --force --since 2026-01-01 --until 2026-03-31
  ```
  Supplying `--since`/`--until`/`--weeks` without `--force` emits a `WARN`
  (the filter is a no-op for new commits but is a footgun without `--force`).

- **Uniform global filter flags on all `tga backfill` subcommands (#332)** —
  `--repos`, `--weeks`, `--since`, and `--until` are now declared as `global`
  flags on `BackfillArgs`, scoping every subcommand (ticket-ids, revert-flags,
  reachability, effort) uniformly. The old per-subcommand `--repo` flag on
  `reachability` is replaced by the global `--repos` (plural, comma-separated).
  ```bash
  tga backfill ticket-ids --repos api --since 2026-01-01
  tga backfill effort --repos core --weeks 8 --force
  tga backfill reachability --repos my-service
  ```

- **Per-repo fetch visibility with `--strict-fetch` and `--verbose-fetch` (#334)** —
  `tga collect` now tracks the outcome of the pre-walk `git fetch` for every
  repository and prints an end-of-run fetch summary to stderr:

  ```
  Fetch summary: 116 / 118 repos updated (2 failure(s), 0 skipped)
    - ml_pricing_engine: could not find remote 'origin'
    - datapipelines: authentication required
  ```

  Successful fetches are omitted from the summary unless `--verbose-fetch` is
  passed. `--strict-fetch` causes `tga collect` to exit non-zero after the
  summary if any repository had a fetch failure — useful for CI pipelines that
  treat stale-data as a hard error.

  When `--no-fetch` is active, a warning is printed to stderr at the start of
  collection:
  ```
  WARNING: --no-fetch active. Local clones may be stale. tga collect will walk
  only what's already in your local object store.
  ```

  The optional per-repo `repositories[].fetch_timeout_secs: <N>` config field
  stores a per-repo fetch timeout in seconds; enforcement via a watchdog thread
  is scheduled for a future release.

- **Comprehensive CLI documentation (#333)** — every subcommand now has:
  - `about` (one-line description shown in `tga --help`)
  - `long_about` (multi-paragraph context shown in `tga <subcommand> --help`)
  - `after_help` with 2-3 concrete examples and `TIPS` lines
  - Per-flag `help` strings on every newly-added flag
  Subcommands with new docs: `analyze`, `classify`, `report`, `backfill`,
  `pr-metrics`, `install`, `aliases`, `override`, `rules`, `deployments collect`,
  `incidents collect`, `dora`.

- **README "Common Workflows" section (#333)** — the README now opens with a
  "Common Workflows" section covering first-time setup, routine weekly runs,
  scoped re-runs, branch-restricted collection, rule tuning, backfill operations,
  and the DORA metrics pipeline. A "CLI Subcommand Reference" table lists every
  subcommand with its purpose and key flags.

### Migration

Existing `tga.db` files were collected with the buggy HEAD-only walker and are
missing commits from non-default branches. To recover the full commit history:

```bash
tga collect --force
```

The `--force` flag bypasses the `collection_runs` skip mechanism, allowing
already-"collected" weeks to be re-walked with the new all-branches coverage.
Expect significantly more commits per week after re-collecting (the bug was
losing ~56% of commits on multi-repo orgs).

### Internal

- New `head_only: bool` field on `GitCollector` and `CollectionPipeline` with
  `with_head_only(bool)` builder method on each.
- Revwalk seeding logic in `extractor.rs::collect_window` now has three arms:
  explicit `branch` override (unchanged), `head_only = true` (legacy escape
  hatch), and the new default (push all `refs/heads/*` + `refs/remotes/origin/*`
  except `refs/remotes/origin/HEAD` which is a symref).
- The all-branch walk falls back to `HEAD` for repos with a detached HEAD and
  no local branches (common in CI shallow clones).
- Regression tests added for all four scenarios: all-branch coverage,
  `--head-only` legacy behaviour, explicit `branch:` override, and
  detached-HEAD fallback (tests in `collect::git::extractor::tests`).

## [1.5.4] - 2026-05-28

### Added

- **`--author <email>` filter on `tga report` (#324)** — Scope any report run
  to a single canonical identity by passing `--author <canonical_email>`.  The
  filter matches case-insensitively against the `canonical_email` field in the
  `authors` table, so `ALICE@EXAMPLE.COM` and `alice@example.com` are
  equivalent.  All formatters (CSV, JSON, Markdown) receive the single-engineer
  `ReportData` unchanged — no formatter logic was modified.  When the supplied
  email does not match any canonical identity, `tga report` exits non-zero with
  a helpful message directing the user to `tga aliases list`.
  Omitting `--author` preserves the existing full-team aggregate behaviour.

## [1.5.3] - 2026-05-27

### Fixed

- **`JiraProjectTier` (Tier 1.6) and `IssueTypeTier` (Tier 1.5) misreport classification method (#319)** — Commits classified via `jira.jira_project_mappings` (Tier 1.6) were persisted with `method = 'regex_rule'` instead of `'external_source'`, causing JIRA project-key-driven verdicts to be attributed to the regex bucket in analytics dashboards. Similarly, PM issue-type-driven verdicts (Tier 1.5, e.g. JIRA `"Bug"` → `bugfix`, Linear `"Story"` → `feature`) were misreported as `'exact_rule'`. Both tiers now correctly emit `ClassificationMethod::ExternalSource` since classification is driven by external ticket-system metadata, not commit-message text patterns. The regression became observable after the 1.5.2 fix to inline `commits.ticket_id` population (#316) — once users could see which commits had JIRA ticket IDs, the misattributed method in `classifications.method` was detectable. No schema migration required; re-running `tga classify` on affected date ranges will repopulate the correct method values.

## [1.5.2] - 2026-05-27

### Fixed

- **`commits.ticket_id` NULL for extractable JIRA IDs during `tga collect` (#316)** — 32.3% of uncategorized commits (2,006 of 6,212) had clearly extractable JIRA ticket IDs in their commit messages (e.g. `BB-2746`, `SRE-3104`, `DRE-405`) but NULL `ticket_id` because extraction only happened in `tga backfill ticket-ids`, not at INSERT time during `tga collect`. `extract_ticket_id` is now called inline during collection so `commits.ticket_id` is populated immediately without a separate backfill pass. The `tga backfill ticket-ids` subcommand is retained for patching legacy databases.

### Changed

- **`extract_ticket_id` moved to `tga::collect::ticket`** — The function was previously a private implementation detail of `src/commands/backfill.rs`. It is now a public API in `tga::collect::ticket` (alongside `is_ticketed`) so it can be reused by the collection pipeline. The `backfill` module now imports from `ticket` rather than maintaining its own duplicate.

## [1.0.12] - 2026-05-19

### Added

- **Native complexity scoring 1–5 with DB migration 0013 and `--backfill-complexity` flag (#97)** — Commits are now scored for complexity on a 1–5 scale using a native classifier. Schema migration 0013 adds the `complexity` column to the `commits` table, and the new `--backfill-complexity` flag allows retroactive scoring of existing commits in the database.

## [1.0.11] - 2026-05-19

### Fixed

- **LLM fallback bypasses cascade short-circuit (#99)** — The four-tier classification cascade was exiting early on exact-rule and regex matches but still invoking the LLM fallback path in some edge cases. The fallback is now gated correctly so it only runs when all preceding tiers produce no result.
- **Gate ADO `merge_commit_sha` by merge strategy (#96)** — The Azure DevOps PR fetcher was writing `lastMergeCommit.commitId` as `merge_commit_sha` regardless of merge strategy. The value is now only persisted when the PR was completed via merge (not squash or rebase), matching the semantics expected by commit-to-PR linkage.
- **Gate GitHub `merge_commit_sha` on `merged_at` (#101)** — The GitHub PR fetcher could write a non-null `merge_commit_sha` for PRs that were closed without merging. The field is now only set when `merged_at` is non-null, preventing phantom commit-to-PR associations.

## [1.0.10] - 2026-05-18

### Fixed
- **Honour `pm.azure_devops.ticket_regex` in work-item extraction (#90)** — The `ticket_regex` field in the ADO PM adapter config block was parsed but not applied during work-item ID extraction from commit messages. The adapter now compiles and uses the user-supplied pattern, consistent with the JIRA, GitHub, and Linear adapters.
- **Persist ADO PR merge commit SHA in `commit_shas` (#92)** — Azure DevOps PRs were fetched and stored without recording the merge commit SHA in the `commit_shas` join table, breaking commit-to-PR linkage. The fetcher now writes the `lastMergeCommit.commitId` value into `commit_shas` when present.
- **ADO PR fetcher supports multiple projects (#91)** — The Azure DevOps PR fetcher previously collected PRs from only the first project in the config. It now iterates over all configured projects, merging results with partial-success semantics (a failure on one project is logged at `WARN` level but does not abort collection for others).

### Added
- **`--force-refresh-prs` flag to backfill ADO `commit_shas` (#95)** — A new `--force-refresh-prs` flag on `tga collect` forces re-fetching of all ADO pull requests regardless of their cached state, enabling operators to backfill `commit_shas` rows that were missing due to the bug fixed in #92.

## [1.0.9] - 2026-05-15

### Fixed
- **Critical data loss bug in `pull_requests` table** (#88): `UNIQUE(provider, pr_number)` constraint
  caused ~62% silent row loss when collecting PRs from multiple repositories (GitHub resets
  `pr_number` per repo). Fixed by adding `repository TEXT NOT NULL DEFAULT 'unknown'` column and
  replacing the unique index with `UNIQUE(provider, repository, pr_number)`.
  Existing databases are migrated automatically (rows default to `repository = 'unknown'`;
  correct values written on next `tga collect` run).

## [1.0.8] - 2026-05-15

### Fixed

- **ADO connectionData probe now uses api-version=7.1-preview.1 (#85)** — The connectivity probe sent during ADO initialization previously used a deprecated API version, causing intermittent auth failures on newer Azure DevOps organizations. The probe now sends `api-version=7.1-preview.1` for consistent compatibility.
- **ADO workitemsbatch now sends errorPolicy=omit (#86)** — The batch work-items endpoint previously returned a full-batch 404 when any single work item ID was invalid or inaccessible. Requests now include `errorPolicy=omit` so valid items in the batch are returned even when some IDs cannot be resolved.

### Added

- **GitHub PR fetcher supports org-wide / multi-repo configs (#87)** — The GitHub PR collection stage now accepts an `org` field and a `repositories[]` list in addition to the existing single-repo `repo` field. Repos are fetched concurrently with partial-success semantics: a failure on one repository is logged at `WARN` level but does not abort collection for the remaining repositories.

## [1.0.7] - 2026-05-15

### Added

- **Bitbucket Cloud PR provider (#72)** — pull-request collection now supports
  Bitbucket Cloud alongside GitHub and Azure DevOps. All providers share a
  `PrProvider` trait and run concurrently via `tokio::task::JoinSet`.
  Configured via a new `bitbucket:` block (workspace, repo_slug, fetch_prs,
  and either a Bearer token or an Atlassian API token via Basic auth).
  Bitbucket PRs are persisted with `provider = 'bitbucket'` so they
  deduplicate correctly against the `(provider, pr_number)` unique index
  from migration `0010_pull_requests_provider.sql`. Bitbucket Server /
  Data Center remains unsupported.

## [1.0.6] - 2026-05-14

### Fixed
- **`llm_fallback_threshold` config field now wired (#78)** — The `classification.llm_fallback_threshold` config value was parsed but never forwarded to `ClassificationEngine`. Commits above the threshold now correctly skip the LLM fallback tier.
- **ADO 404 error message clarified (#80)** — Azure DevOps 404 responses previously surfaced a generic HTTP error. The message now identifies the missing resource and suggests checking the project/organization slug in config.
- **`ticket_regex` config wired for JIRA, GitHub, and Linear adapters (#75)** — The `ticket_regex` field defined in each PM adapter's config block was ignored; all three adapters now compile and apply the user-supplied regex pattern when detecting ticket references in commit messages.
- **ADO workitemsbatch omit policy: dropped IDs now logged (#81)** — When the ADO batch API silently omits work item IDs (e.g. items in areas the token cannot read), `tga` now logs each dropped ID at `WARN` level so operators can identify access-control gaps without digging through raw HTTP responses.

### Performance
- **Parallel LLM fallback with `buffer_unordered` (#83)** — The per-commit LLM classification loop was fully serial; each request waited for the previous one to complete. Requests are now dispatched concurrently using `futures::stream::buffer_unordered`, capped at the configurable `llm_fallback_concurrency` limit (default 4), cutting wall-clock classification time roughly proportional to API latency.

### Added
- **ADO pull request fetcher with reviewer tracking (#84)** — `AzureDevOpsClient` now implements `fetch_pull_requests`, collecting PR metadata (title, state, author, dates, target branch) and the full reviewer list (identity, vote, required flag) into the `pull_requests` and `pr_reviewers` tables (migration `0011_pr_reviewers.sql`). Enabled per repository via `pm.azure_devops.fetch_prs: true`.

### Tests
- **Ticket-regex detection coverage (#76)** — Added unit tests for `ticket_regex` detection across the JIRA, GitHub, and Linear adapters, covering match, no-match, and malformed-pattern cases.

## [1.0.5] - 2026-05-12

### Fixed
- **GitHub PR fetch resilience (#73)** — `fetch_pull_requests` now routes through the same `retry_request` helper used by every other paginated GitHub endpoint. A single 429 or 5xx response no longer fails the entire PR collection run; transient errors are retried with exponential backoff.
- **Pull-request deduplication (#71)** — `INSERT OR REPLACE INTO pull_requests` was a silent no-op: the existing `idx_pull_requests_pr_number` index was non-UNIQUE, so SQLite's conflict resolution never fired and PRs accumulated on every re-run. Migration `0010_pull_requests_provider.sql` adds a `provider` column (default `'github'`) and a UNIQUE index on `(provider, pr_number)`. `store_pull_requests` now writes the provider explicitly so deduplication works correctly per provider.

## [1.0.4] - 2026-05-12

### Added
- **Self-analysis example config** (`configs/self-analysis.yaml`) — a minimal config that runs `tga` against its own repository, useful as a quickstart template and for dogfooding releases.

### Changed
- Documentation cleanup pass: refreshed `CLAUDE.md` implementation state date, verified ADR README and README.md version references are current.

## [1.0.3] - 2026-05-12

### Fixed
- **Bedrock LLM provider no longer warns about missing API key** — Bedrock uses the AWS credential chain; API key validation now correctly skips for the `bedrock` provider.

### Added
- **ADR documentation** — `docs/adr/README.md` explains the format and process; `docs/adr/TEMPLATE.md` provides a copy-paste starter. Format is referenced from `docs/requirements/rust-architecture.md` and `tga --help`.

## [1.0.2] - 2026-05-12

### Fixed
- **Timezone-aware ISO week assignment** (#70): commits timestamped late on the last day of an ISO week in a negative UTC offset timezone (e.g. -0700) were incorrectly placed in the following week due to UTC conversion. Collection window now uses the commit's local calendar date.
- **`until_date` inclusivity**: date range upper bound is now treated as end-of-day inclusive.

## [1.0.1] — 2026-05-12

Final pre-release polish: data-quality guards for multi-repo analysis,
identity-resolution honesty, and a clean CI gate.

### Added

- **Multi-repo coverage warning (#67)** — `ReportData` now carries a
  `repository_coverage` field counting the distinct `commits.repository`
  values observed in a run. The `analyze` command warns when this is
  smaller than the configured `repositories[]` roster, so a misconfigured
  scope no longer silently undercounts the portfolio.
- **Phantom-developer guard (#68)** — `unresolved_authors` and
  `unresolved_author_commits` are now tracked separately from the headline
  developer counts. Commits whose author identity does not resolve to a
  configured canonical team member are surfaced rather than inflating
  active-developer tallies.
- **WoW baseline drift detection (#69)** — `collection_runs` gains a
  `repo_count` column (migration `0009_collection_runs_repo_count.sql`)
  recording the size of `repositories[]` at the moment each row was
  written. Week-over-week comparisons can now detect when the prior
  baseline week was collected against a different repository roster and
  refuse to draw spurious deltas.
- **Restored `--log` flag (#66)** — `tga --log <path>` re-introduces the
  pre-consolidation log redirection contract; all `tracing` output is
  written to the supplied file in addition to stderr.

### Fixed

- **Clippy `unnecessary_sort_by`** — 8 closures of the shape
  `sort_by(|a, b| a.field.cmp(&b.field))` rewritten as `sort_by_key`,
  using `std::cmp::Reverse` for descending sorts. `cargo clippy
  --all-targets -- -D warnings` is now clean.
- **Rustdoc broken intra-doc links** — Fixed eight stale links left over
  from the workspace consolidation. All references to `crate::models`,
  `crate::aggregator`, and `crate::formatters` now point at their
  consolidated paths under `crate::report::*`; bare `TopLevelCategory` and
  `CollectError` references are fully qualified; private items
  (`Self::members`, `Database::apply_pragmas`) use code formatting instead
  of doc-links. `cargo doc --no-deps` is now warning-free.
- **`duetto_contractors_config_resolves` identity resolver test** —
  Verified passing against the bundled `configs/duetto-contractors.yaml`
  fixture; "Andre Ramos" and the other listed contractors resolve
  correctly via the configured alias map.

### Documentation

- `docs/requirements/database-schema.md` — Added the `collection_runs`
  table definition and the new `repo_count` column; documented the Rust
  port's re-indexed migrations (`0001`–`0009`).
- `docs/requirements/reporting.md` — Documented the new
  `repository_coverage`, `unresolved_authors`, and
  `unresolved_author_commits` fields on `ReportData`.

## [1.0.0] — 2026-05-12

First stable release. `trusty-git-analytics` is now feature-complete as a Rust port of
[`gitflow-analytics`](https://github.com/bobmatnyc/gitflow-analytics): every analytical
output the Python tool produces is reproduced by `tga` from the same YAML config, against
the same SQLite schema, with materially better performance and a single static binary.

### Highlights

- **Single `tga` crate** — consolidated from the original 5-crate workspace into one
  library + binary for faster builds, simpler dependency graph, and a cleaner public API.
- **8 CLI subcommands**: `analyze`, `collect`, `classify`, `report`, `init`,
  `validate-config`, `migrate`, `override` (plus auxiliary `aliases`, `backfill`,
  `pr-metrics`, `identities`, `install`, `fetch`).
- **7-tier classification cascade**: manual override → issue type → JIRA project mapping
  → exact (Aho-Corasick) → regex → fuzzy heuristics → LLM fallback, with both **AWS
  Bedrock** and **OpenRouter** as supported LLM providers (Bedrock behind the `bedrock`
  cargo feature).
- **9 CSV reports** plus DORA, velocity, and quality summaries:
  `weekly_metrics`, `developer_activity_summary`, `summary`, `untracked_commits`,
  `weekly_categorization`, `weekly_velocity`, `weekly_dora_metrics`, `authors`,
  `weekly_activity` — with `velocity_summary.json`, `quality_summary.json`, and
  `dora_summary.json` alongside.
- **Full data collection layer**: libgit2 commit extraction, identity resolution
  (exact + Jaro-Winkler fuzzy + email local-part normalized), GitHub REST client
  (PRs, reviews, commits, issues), JIRA Cloud client (JQL batch search, story points),
  Linear GraphQL client, Azure DevOps REST client (work items, iterations, users,
  comments, custom fields, commit links).
- **SQLite with WAL** on every open: `journal_mode=WAL`, `synchronous=NORMAL`,
  `foreign_keys=ON`, `cache_size=-65536`, `temp_store=MEMORY`, `mmap_size=256 MiB`.
- **Schema migration runner** applying versioned SQL migrations v1–v18 (ported from
  Python predecessor) plus Rust-era additions, recorded transactionally in
  `schema_migrations`.
- **247 tests passing** — unit, integration, and doctests — with zero clippy warnings
  and `cargo fmt --check` clean.
- **Cross-platform release binaries** published from CI for five targets:
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

### Compatibility

- Config files written for `gitflow-analytics` load without modification — unknown keys
  are silently ignored.
- The on-disk SQLite schema is the same shape; existing `gitflow_cache.db` files from
  `gfa` can be opened by `tga` and auto-migrated forward.
- Classification rule files (YAML/JSON) are portable in both directions.

## [0.3.0] — 2026-05-12

### Added
- **Classification**: Tier 0 manual override lookup (`classification_overrides` table), Tier 1.5 issue-type classifier (12-entry map, 0.90 confidence), Tier 3 JIRA project key mapping (0.95 confidence), classification coverage metrics persisted to DB, `--validate-coverage` flag
- **LLM**: OpenRouter provider with required headers (`HTTP-Referer`, `X-Title`); AWS Bedrock provider feature-gated (`--features bedrock`, model `anthropic.claude-3-haiku-20240307-v1:0`)
- **Reports**: 9 CSV reports (weekly_metrics, developer_activity_summary, summary, untracked_commits, weekly_categorization, weekly_velocity, weekly_dora_metrics), DORA 4-band classifier, velocity/PR cycle time, composite activity scoring, quality/revert detection, boilerplate filter; `velocity_summary.json`, `quality_summary.json`, `dora_summary.json`
- **GitHub client**: `fetch_pr_reviews`, `fetch_pr_commits`, `list_issues`; exponential backoff on 5xx/429
- **JIRA client**: `search_issues` (batch JQL, 50/page), `get_story_point_field`; `JiraIssue.story_points`
- **Azure DevOps Phase 4**: `get_iterations`, `get_users`, `feed_azdo_users`→IdentityResolver; `azdo_iterations` table (migration v8)
- **Azure DevOps Phase 5**: `get_work_item_comments`, `get_work_item_extended` (custom fields, iteration/area path), `get_work_item_commit_links`
- **CLI**: `tga override add|list|remove`, `tga pr-metrics`, `tga install` wizard, `tga aliases list|merge`, `tga backfill ai-detection|revert-flags|ticket-ids`
- **git remote fetch** before revwalk with non-interactive SSH auth; `--no-fetch` to skip
- **`--from`/`--to`** date flags on `collect` and `analyze`; mutual exclusivity with `--weeks` enforced
- **`--dry-run`** on `collect` and `analyze` — routes writes to in-memory DB
- **`--validate-only`/`--no-validate`** flags; `ConfigValidator` with 9 error variants
- **SQLite tuning**: `cache_size=-65536`, `temp_store=MEMORY`, `mmap_size=268435456`, `synchronous=NORMAL`, `foreign_keys=ON`; documented in `docs/adr/0001-sqlite-tuning.md`
- **Criterion benchmarks**: 5 groups in `benches/tga_bench.rs` (classify throughput, aho-corasick, CSV gen, identity resolution, ISO weeks)
- **Cross-compilation** release workflow: 4 targets (x86_64/aarch64 Linux musl, macOS Intel/Apple Silicon)
- `docs/migration-from-python.md`, `docs/adr/0002-performance-hotspots.md`

### Fixed
- `since_date` config / `--weeks` flag now correctly limits git revwalk (was collecting full history)
- `weekly_fetch_status` incremental skip now works — bounded path entered for `(since, None)` range
- Warning + spinner emitted when full-history traversal is about to occur

## [0.2.0] — 2026-05-11

### Added
- Unified `PmAdapter` trait abstracting JIRA, GitHub, Linear, and Azure DevOps behind a common `fetch_ticket` / `detect_ticket_refs` / `health_check` interface (`src/collect/pm_adapter.rs`)
- Azure DevOps Phases 3–6: work item types, WIQL queries, batch fetch (200/chunk), AB# reference detection and enrichment; `AzureDevOpsAdapter::fetch_ticket` now fully implemented
- `--weeks N` flag on `collect` and `analyze` subcommands — limits collection to the last N weeks (overrides config `start_date`)
- `GitHubAdapter::fetch_ticket` — real implementation fetching individual GitHub Issues via REST API (was stub)
- `--dry-run` flag on `collect` and `analyze` — runs the full pipeline against an in-memory DB, making no on-disk writes
- SQLite persistence for work items: `work_items` table (migration v5), `commit_work_items` join table, and `upsert_work_item` / `link_commit_work_item` / `get_work_items_for_commit` / `list_work_items` DB operations

### Changed
- `AzureDevOpsAdapter::fetch_ticket` no longer stubs — returns populated `PmTicket` from batch work item fetch

## [2026-05-11]

### Added

#### `src/collect/pm_adapter.rs` — unified PM adapter layer
- `PmAdapter` async trait (`fetch_ticket`, `fetch_tickets`, `detect_ticket_refs`, `health_check`) with `Send + Sync` bounds for use behind `Box<dyn PmAdapter>`
- `PmTicket` normalized ticket payload preserving the raw upstream JSON in `PmTicket::raw` for forward compatibility
- `PmSource` enum (`Jira`, `GitHub`, `Linear`, `AzureDevOps`) with stable `as_str()` labels
- `PmError` thiserror enum covering `Http`, `Auth`, `NotFound`, `RateLimited`, `Serialization`, `Config`, `Other` variants
- Concrete adapter wrappers: `JiraAdapter`, `GitHubAdapter`, `LinearAdapter`, `AzureDevOpsAdapter` — each delegates to the existing backend client
- `build_adapters(config)` factory: instantiates every adapter whose config section is present; skips missing or invalid sections with `tracing::warn!` rather than failing the whole pipeline
- Shared detection regexes: JIRA/Linear `KEY-N`, GitHub `#N` (non-hex), ADO `AB#N`

#### `src/collect/azdo/client.rs` — Azure DevOps Phases 3–6
- `get_work_item_types(project)` — fetch all work item types defined in a project (Phase 3)
- `get_fields(project)` — fetch all field definitions for a project (Phase 3)
- `run_wiql(project, query)` — execute an arbitrary WIQL query, returning `WiqlResult` with work item IDs and refs (Phase 4)
- `get_recent_work_item_ids(project, since)` — convenience WIQL wrapper for recently-updated items (Phase 4)
- `get_work_items(ids)` — batch-fetch work items by ID; chunks requests at 200 IDs per call per ADO API limits (Phase 5)
- `extract_work_item_refs(text)` — standalone function extracting bare `AB#N` integers from commit messages (Phase 6)
- `fetch_referenced_work_items(client, text)` — convenience wrapper: detects refs in text, fetches the work items, returns `Vec<AzdoWorkItem>` (Phase 6)
- `AzureDevOpsAdapter::fetch_ticket` is now fully implemented (strips `AB#` prefix, calls `get_work_items`, maps to `PmTicket`)

#### `--weeks N` flag (`src/main.rs`, `src/commands/collect.rs`, `src/commands/analyze.rs`)
- `--weeks <N>` added to both `tga collect` and `tga analyze` subcommands
- Computes `now − N weeks` as an RFC3339 lower bound and applies it to every configured repository's `since_date`, overriding any `start_date` in config
- `--since` (explicit ISO date) takes precedence over `--weeks` when both are supplied on `collect`

### Added

#### tga-core
- `Config` struct with full YAML deserialization via `serde_yaml`; compatible with the Python `gitflow-analytics` config schema
- `Config::load()` with tilde-expansion on all path fields
- `Config::validate()` enforcing at minimum one configured repository
- `Config::resolved_aliases()` unifying `developer_aliases` (Python-compat flat map) and `team.members` (structured roster)
- `RepositoryConfig`, `TeamConfig`, `TeamMember`, `OutputConfig`, `ClassificationConfig`, `GithubConfig`, `JiraConfig`, `AnalysisConfig`, `CacheConfig` structs
- `Database` wrapper with mandatory WAL journal mode, `synchronous=NORMAL`, and `foreign_keys=ON` applied on every open
- `Database::open()` and `Database::open_in_memory()` with automatic migration execution
- `Database::journal_mode()` and `Database::schema_version()` introspection helpers
- Versioned SQL migration runner (`db::migrations`): transactional application, idempotent, records `(version, name, applied_at)` in `schema_migrations`
- Migration v1 (`0001_initial_schema.sql`): creates `authors`, `classifications`, `commits`, `files`, `pull_requests`, and `schema_migrations` tables with appropriate indexes and foreign keys
- Domain models: `Commit`, `Author`, `Classification`, `FileChange`, `PullRequest`
- Enums: `ClassificationMethod` (`exact_rule`, `regex_rule`, `fuzzy_match`, `llm_fallback`, `manual`), `ChangeType` (`added`, `modified`, `deleted`, `renamed`), `PrState` (`open`, `closed`, `merged`)
- `TgaError` enum with `thiserror` covering I/O, SQLite, YAML, validation, and migration errors
- `tga_core::Result<T>` type alias
- 100% `#[warn(missing_docs)]` coverage; `cargo doc` passes with `RUSTDOCFLAGS="-D warnings"`

#### tga-collect
- `CollectionPipeline` orchestrator: sequential per-repo git extraction, author backfill, optional GitHub PR fetch
- `CollectionStats` reporting commits collected, authors resolved, PRs fetched, and per-repo non-fatal warnings
- `GitCollector`: opens a local repository via libgit2, validates existence and git-ness on construction, walks the default branch, extracts commit SHA/author/timestamp/message/diff-stats
- `IdentityResolver` with three-tier resolution: (1) exact alias match on email, (2) exact alias match on name, (3) Jaro-Winkler fuzzy match (default threshold 0.85), (4) raw passthrough
- `IdentityResolver::from_config()` preferring `developer_aliases` over `team.members`
- `IdentityResolver::upsert_author()` writing canonical rows to the `authors` table with `ON CONFLICT` upsert
- `GitHubClient`: REST client for fetching pull requests via `reqwest` + `tokio`; stores PR rows via `store_pull_requests()`
- JIRA client stub (`JiraClient`) for future issue fetch integration
- `CollectError` enum with `thiserror`

#### tga-classify
- `ClassificationEngine` combining all four tiers behind a single `classify()` async entry point and a `classify_batch()` Rayon-parallel sync entry point
- `ClassificationEngineConfig` with `use_llm`, `llm_model`, and `confidence_threshold` fields
- `ExactMatcher`: builds a single Aho-Corasick automaton from all rule keyword lists; O(n) scan per message
- `RegexMatcher`: compiles one `Regex` per pattern string; first match wins; extracts JIRA ticket IDs via `extract_ticket_id()`
- `FuzzyClassifier`: heuristic detection of merge commits (via `is_merge` flag or "Merge pull request" prefix) and revert commits (via "Revert" prefix)
- `LlmClassifier`: optional async OpenAI-compatible fallback; reads `OPENAI_API_KEY` from environment; silently no-ops if key is absent
- `ClassificationResult` struct carrying `category`, `subcategory`, `confidence`, `method`, `ticket_id`
- Built-in default ruleset (`default_rules()`): 15 rules covering conventional commit prefixes (`feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `ci`, `perf`, `style`, `build`, `revert`), breaking-change marker, JIRA-style ticket pattern, and keyword fallbacks (`bug`, `security`)
- `load_rules()`: YAML or JSON rules file loader (format detected by extension)
- `Rule` and `RuleSet` types with `id`, `category`, `subcategory`, `keywords`, `patterns`, `priority`, `confidence`
- `ClassificationPipeline` orchestrator: reads unclassified commits from DB, runs cascade, writes `classifications` rows, links `commits.classification_id`
- `ClassificationStats` with `total_commits`, `classified`, `by_method`, `by_category` breakdowns

#### tga-report
- `Aggregator`: reads `commits` + `classifications` + `authors` from DB and produces an in-memory `ReportData`
- `ReportData` with `generated_at`, `period_start`, `period_end`, `total_commits`, `total_authors`, `category_breakdown`, `authors`, `repositories`, `weekly_activity`
- `AuthorSummary` per-author rollup: name, email, commit count, insertions, deletions, files changed, category map, first/last commit timestamps
- `RepositorySummary` per-repo rollup: name, commit/author counts, insertions, deletions, top categories
- `WeeklyActivity` per-week/author/repo bucket: ISO week label, counts, insertions, deletions, category map
- CSV formatter: `write_author_csv()` → `authors.csv`, `write_weekly_csv()` → `weekly_activity.csv`
- JSON formatter: `write_json()` → `report.json` (serializes `ReportData` directly)
- Markdown formatter: `write_markdown()` → `report.md` via embedded Tera template
- `ReportPipeline` orchestrator: resolves output directory, dispatches to all configured formatters, returns `ReportStats` with files written
- Default output directory `./reports` when `output.directory` is unset
- All three formats emitted when `output.formats` is empty

#### tga-cli
- `tga` binary with four subcommands wired to the pipeline crates
- `tga analyze`: runs collect → classify → report in sequence; `--skip-collect` and `--skip-classify` flags for partial re-runs; `--output` override
- `tga collect`: `--repos` filter, `--since` / `--until` date overrides
- `tga classify`: `--rules` file override, `--use-llm` flag
- `tga report`: `--output` directory override, `--formats` comma-separated list
- Global `--config` (default `config.yaml`), `--database` (default `tga.db`), and `-v`/`-vv`/`-vvv` verbosity flags
- `tracing-subscriber` initialization from verbosity count: `WARN` / `INFO` / `DEBUG` / `TRACE`
- Graceful config-not-found fallback to `Config::default()`
- `anyhow::Result` error propagation to `main`

#### CI / CD
- GitHub Actions CI workflow (`ci.yml`): runs on push and PR to `main`; matrix over `stable` and `beta` toolchains; jobs: format check, Clippy with `-D warnings`, tests (skipping the integration test that requires a local git repo configured via `INTEGRATION_REPO_PATH`), rustdoc build with `RUSTDOCFLAGS="-D warnings"`, release binary build
- Concurrent-run cancellation via `concurrency.cancel-in-progress`
- Rust artifact caching via `Swatinem/rust-cache@v2`
- GitHub Actions publish workflow (`publish-tga-core.yml`): triggered by `tga-core-v*` tags or `workflow_dispatch`; dry-run gate, Clippy gate, then `cargo publish`; supports `dry_run` input to skip actual upload
- `CARGO_REGISTRY_TOKEN` secret required for actual publish

#### Integration
- `configs/example-config.yaml`: example config for analyzing multiple repositories with developer aliases, CSV + Markdown output
