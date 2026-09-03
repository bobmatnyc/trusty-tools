# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.33.0] — 2026-09-03

### Fixed

- Report prose follows the rendered data and the recorded provenance
  - The synthesis digest now states the complexity distribution in the same
    string the Code Quality table renders, and a Code Quality paragraph claiming
    no complexity data was provided is dropped with a Synthesis Status
    disclosure when the table renders one — matched on the absence claim itself,
    so a paraphrase ("complexity could not be measured") is caught too, and
    scoped per application, so a true claim about an application that measured
    nothing survives (#6691)
  - The digest no longer reports a metrics artifact's zero file/function counts
    as measurements; it falls through to the repository scan, as the Key Facts
    block already did (#6691)
  - The CAST report's §1 "Analysis methodology" row is generated per run from
    what actually contributed — trusty-analyze metrics per application and
    trusty-search trace anchors — instead of fixed template text asserting both
    tools were used (#6675)
- `--analyze` sizes each request to the work the endpoint actually does
  - `analyze.complexity_distribution` and `analyze.refactor_suggestions` grade
    every chunk of the index rather than reading computed data, which takes
    41-46 s on a 104k-chunk checkout against the 15 s they were allowed. Both
    now get a corpus budget defaulting to 180 s, so the §7 complexity table is
    filled instead of rendering `not stated in source data` on every large
    repository (#6712)
  - `--analyze-timeout-secs` and the manifest key `[report].analyze_timeout_secs`
    set that budget, in that order of precedence; it also raises the diagnostics
    budget when it exceeds that endpoint's own deadline ladder. The readiness
    probe keeps its 15 s, so an unreachable daemon is still declared unreachable
    in seconds (#6712)
  - A completed analyze request logs its elapsed time at INFO, and a request
    that runs out of time still names the budget it exhausted (#6712)
- The `analyze_enrich` module's doc comment links resolve again — the split that
  created it wrote them as `super::`, which rustdoc reads from the crate root
  rather than from `report/`, so `cargo doc` failed on two broken intra-doc
  links (#6712)

## [0.32.0] — 2026-09-02

### Added

- `report --code-only` renders the CAST template's non-code sections as stated out-of-scope boundaries instead of dropping them, and marks the code-derived-but-uncorroborated ones as inferred; a template declares its own regions with `code_only:non_code` / `code_only:partial` markers. `--template` gains the `cast` / `default` / `generic` aliases, the manifest gains `[report] code_only`, and the report pipeline moves into the library as `trusty_review::report::run_report` so `trusty-analyze report` drives one implementation rather than a second copy. The CAST template also gains the Code Quality & Architecture / Security Posture / Performance & Scalability sections deferred under #6004, marked trusty-derived rather than CAST-scored, plus a Next Steps section and an Audit scope metadata row. Code-only regions never nest: a region that opens another region before its own `code_only:end` is left untransformed and logged, so a nested marker can no longer close the outer region early and spill the rest of its body into the report as literal template text. (#6669)
- `report::mermaid::tests::{parses_full_marker, ignores_non_dataset_comment}` — `parse_marker` had no direct coverage, so nothing proved the `group:` field survives the comma-inside-`y:` grammar or that a marker missing `x:`/`y:` is rejected rather than half-parsed (#6678).
- `report::reporter::tests::a_second_narrative_cannot_overwrite_a_claimed_row` — two narratives sharing one title and one cited file exercise `match_row`'s unclaimed-rows filter on its own, which component disambiguation cannot reach (#6678).

### Fixed

- The review runner rendered the full noise-filtered diff twice (once unbounded just to measure its length, once bounded for the prompt) — now renders once; the map-reduce decision reuses the bounded render's own truncation marker (Refs #1660).
- The map-reduce branch assigned `ReviewStatus::Degraded` from two independent sites (partial coverage, opted-out context dependency) — consolidated to one assignment so a future finer-grained status can't be silently clobbered by whichever site ran second (Refs #1661).
- The calibration harness's `run_pipeline_for_entry` built its own `GithubClient`/token instead of reusing the runner's shared `resolve_diff_token` helper — now routes through the same funnel, removing an auth-path divergence risk (Refs #1632).
- `compute_metrics` panicked via `assert_eq!` on a corpus/results length mismatch — now returns a `Result` error instead (Refs #1632).
- `report::exec_summary`'s `# Spec References` block names DOC-67 by its
  repo-root-relative path rather than a `../../../../` traversal, so its
  reference is checked by `check_sld.sh` instead of skipped (#6605).
- The report pipeline finds a checkout's trusty-search index when it is
  registered under an id the path does not derive to. `--analyze` and the trace
  pass resolved by `derive_checkout_index_id` and an exact id match only, so a
  ready index registered at the same `root_path` under another id — the main
  checkout served as `trusty-tools-checkout` against a derived
  `trusty-tools-4e2cf878` — could never be reached: analyze degraded to scan,
  every trace lookup returned `IndexAbsent`, and the run exited 0. Both call
  sites now resolve through `report::index_registry::resolve_report_index`,
  which keeps the derived id when the daemon holds it, otherwise substitutes the
  index whose canonicalised `root_path` IS the checkout (logging the
  substitution), and otherwise names the derived id and says nothing registered
  covers the path (#6677).
- `HttpAnalyzeMetricsSource::with_search_base_url` points the trusty-search
  registry read at an explicit address. Without it the read resolved the
  machine's advertised daemon whatever socket the source was built with, so a
  caller holding the source over a stub analyze socket still issued a live HTTP
  GET to whatever trusty-search was running and resolved against what that
  daemon held (#6677).
- The report's unresolved-index warning says which cause it hit —
  `registry=empty` for a daemon that answered nothing, `registry=populated` for
  one holding indexes none of which is rooted at this checkout — so the remedy
  reads off one line instead of two (#6677).
- The required-context gate now decides from `GET /indexes/{id}/status` for the index under review instead of the host-wide `/health` counters, so one failed index anywhere on the host no longer degrades every review and stamps a NOT AUTHORITATIVE banner on it. `/health` retains one job: is the daemon reachable and serving. A degraded target index still labels the review, and its reason names that index and the lanes that actually failed rather than inferring them from counter arithmetic that could claim lexical results were available when the lexical lane had failed too (#6686).
- A review whose configured trusty-search index does not exist now skips loudly and names the index, instead of collapsing the daemon's `404 unknown index` into an empty result set and publishing an authoritative verdict with no code context. A query that fails against an index that does exist stays fail-open. `SubprocessAnalyzeClient::has_analysis` reads its `index_id` argument and reports no analysis for an index trusty-search has never heard of (#6687).

### Documentation

- Repointed seven `Test:` citations whose named test had been renamed, and rewrapped the `analyze_endpoints` module citation that split an identifier across two doc lines. All were invisible while the pointer lint dropped brace-set citations (#6678).
- Repointed eighteen more brace-set `Test:` citations across `context_gate`, `analyze_findings`, `investigate::select`, `investigate::verify`, `mermaid`, `reporter`, `reporter_findings`, `synthesize`, `synthesize_normalize`, `synthesize_prompt` and `topology` at the tests that exist — renamed successors where the test was renamed, and the surviving test where the cited name never shipped (#6678).
- Corrected `verify_findings`'s What paragraph, which still described GREEN as a title-only topic admitted without evidence; since #6080 every band must cite a file in the inspected set (#6678).

## [0.31.0] — 2026-08-31

### Removed

- APEX/KB retrieval no longer runs during a review. The `apex_context` module,
  the `apex_index` / `apex_path_prefixes` config surface (env vars
  `TRUSTY_SEARCH_APEX_INDEX` / `TRUSTY_REVIEW_APEX_PATH_PREFIXES`, now ignored),
  the `[apex: …]` inline-citation form, and the `ReviewContext::apex_results`
  field are gone. APEX context was cited 0 times across 69 audited findings at
  ~0.001 relevance, so the owner ruling dropped the retrieval step rather than
  forcing citation (#4999). GitHub and JIRA/Confluence context retrieval are
  unchanged.

## [0.30.0] — 2026-08-31

### Breaking

- `HttpAnalyzeClient` is removed. Its one remaining caller, the non-default
  `review` feature's `build_review_state`, now builds a `SubprocessAnalyzeClient`,
  which needs no daemon at all. The `AnalyzeClient` trait and its response types
  are unchanged.
- `ReviewConfig::analyzer_url` becomes `analyzer_socket: PathBuf`, and
  `DEFAULT_ANALYZER_URL` becomes `default_analyzer_socket()`. The environment
  variable is `PR_INTELLIGENCE_ANALYZER_SOCKET`, not
  `PR_INTELLIGENCE_ANALYZER_URL`.
- `report::HttpAnalyzeMetricsSource::new` takes a socket path instead of a base
  URL, and `AnalyzeAdapterError::Api { status, body }` becomes
  `Rpc { code, message }`.

### Added

- `calibrate` measures #1897's three verdict-level acceptance bars: strict verdict agreement, RC-recall, and the clean-PR over-flag rate. The harness previously discarded `ReviewResult::verdict` and reported only finding-level recall/precision, so those bars had no expressible answer. A corpus entry opts in with `reference_verdict`; a corpus with none reports "not measured", and no bar can pass on an empty denominator (#2974).
- Report manifests may declare a per-repository `inspect_priority` list — ranked repo-relative paths, optionally with an explicit `weight` — that the DD-report investigation inspects ahead of its own path-name heuristics, still bounded by the same file/byte budget. This is the interface an external ranker (trusty-audit) writes its selection intelligence to, so tuning what gets inspected no longer changes trusty-review. Absent the key, selection is byte-identical to before.
- File selection now folds in the trusty-analyze findings already loaded for a repository: a file a diagnostic or refactor suggestion names outranks an otherwise-equal file the heuristics cannot tell apart.
- The Investigation Coverage line and the synthesis-prompt coverage summary state the examined share as a percentage alongside the raw counts.
- `[report].attributed_only` in the manifest restricts the investigation pass to the files `inspect_priority` declares. `inspect_priority` was a dominant sort key rather than a filter, so a declared list shorter than the file budget was topped up with path-name-scored files — files carrying no dimension and no reason, counted toward the examined set exactly as search-found evidence was. Under the flag the remaining budget goes unspent and the coverage section states the shortfall: `attributed-only selection: N of M budgeted file(s) carried search or complexity evidence; the remaining K were left unread rather than filled by path-name heuristics`.
- The flag is opt-in and manifest-only, with no CLI flag: a manifest written without a search index still wants the heuristic top-up, and only the producer that reached a healthy index knows whether the declared list is the whole intended sample. A repository whose `inspect_priority` is empty ignores the flag rather than examining nothing.
- A manifest `inspect_priority` entry may now declare `dimension` and `reason` alongside its path: which due-diligence dimension the file is evidence for, and the query or measurement that found it. A declared dimension counts toward the report's dimension coverage, so a dimension no path-name heuristic could recognise is covered when the ranker says the file is evidence for it.
- The Investigation Coverage section states, per repository, how many examined files came from manifest-declared evidence queries and how many files each covered dimension got, with one example naming why that file was read.
- The verified investigation is written to `investigation.json` in the report's output directory before synthesis begins, so a synthesis failure no longer discards the run's expensive half.
- `report --manifest` resolves the provider and per-role models from the
  manifest's new `[inference]` section, ahead of this host's environment and
  `~/.config/trusty-review/config.toml`. Precedence per field is CLI flag >
  manifest > env var > config file > built-in default, so a delivered audit
  package re-renders on the provider that produced it — the failure this
  replaced was a June-dated local `provider = "bedrock"` hijacking an OpenRouter
  engagement's render. A manifest with no `[inference]` section (every manifest
  written before this) resolves exactly as it did before. The section carries
  identity only: a credential still comes from the environment, and a render
  whose declared provider has no key stops before any repository is walked, with
  the provider named in the message.
- The report states which models ran. Its Report Metadata table gains an
  `Inference models` row and the JSON twin an `inference` object, both listing
  provider and per-role model — and both showing `requested → ran` wherever the
  resolver adjusted an id.
- The investigation prompt names the function to look at. An `inspect_priority`
  entry may now declare `hotspot = { function, start_line, end_line, cyclomatic
  }` — trusty-audit writes it from the complexity measurement (#6145) — and the
  batch digest states it directly above that file's fenced content:
  `Hotspot: lines 40-190, fn settle_invoice, cyclomatic 31 — prioritize DD
  analysis of this function.` The line comes before the content, so the model
  reads the instruction rather than reaching it after skimming 900 lines. The
  Investigation Coverage section names the function too, beside the reason the
  file was read.
- A file with no declared measurement produces a byte-identical prompt, and a
  measurement without a usable line range produces no line at all rather than
  one pointing at lines 0-0. A half-written `hotspot` table is inert, the same
  way a priority naming no tracked file is inert — it never fails the manifest
  load. Nothing here changes which files are selected.
- Code Quality & Architecture gains a deterministic crate-topology table for any
  repository whose manifest declares one (trusty-audit measures it from cargo
  metadata). A summary line states the member count, the internal dependency
  edge count, the cycle verdict, and the most-depended-on crates; the table lists
  up to 15 crates — most depended on and shallowest first, so the shared core
  leads — with each one's direct internal dependencies and inbound count. The
  synthesised paragraph renders above it and is told to comment on that
  structure rather than re-derive one.
- The same facts reach the synthesis prompt as a measured block, and the numbers
  are admitted by the numeric guardrail because they are rendered into the fill
  scope the guardrail reads — so the paragraph may quote a member or inbound
  count without the guardrail rejecting the whole field.
- A repository that declares no topology renders a report byte-identical to one
  produced before this existed: the block is omitted whole, never filled with
  honesty markers, and the prompt gains no heading.
- Every RED finding and the first ten AMBERs now carry a symbol-graph anchor in `investigation.json`: the symbol its `file:line` citation resolves to, the declaration trusty-search holds for that symbol, and its usage sites inside the same file. A reader who wants to act on a finding gets the declaration without opening the repository. A candidate that cannot complete every step records a `no trace:` reason instead — an unreachable daemon, an unregistered index, a citation with no item declaration, a symbol the graph does not hold, or an entry the graph placed in a different file — and never a partial or guessed anchor.
- The Investigation Coverage section states `traces assembled: X of Y candidate findings (Z no-trace)`. A report run before this pass renders byte-identically.
- Call edges are deliberately absent: `GET /indexes/{id}/call_chain` resolves callees and callers by bare symbol name, so on this workspace 254 of one symbol's 321 callee edges cross a crate boundary, 16 land in non-Rust files, and a bare `get` collects 2391 caller edges. Each trace carries `call_edges: []` and a `call_edges_status` naming #6167 as the blocker.
- Every traced finding now gets a verdict from the verifier-role model, judged against its own trace: CONFIRMED annotates the finding with the anchor declaration and usage count it was checked against, CLEARED drops it one severity band and appends the verifier's one-line reason, UNVERIFIABLE changes nothing. A cleared finding is never removed — a cleared AMBER lands in the clean-signals list naming the clearing, so it cannot read as a strength the investigation found.
- The Investigation Coverage section states `trace verdicts: A confirmed, B cleared, C unverifiable of D traced`, names the model that judged them, and lists every cleared and unverifiable finding with its reason.
- Gaps & Caveats carries one line per repository whose traced findings could not all be checked, so a reader who skips the coverage section still cannot read an unverified finding as a disproved one.
- The pass fails closed at every point: a transport error, a timeout, a truncated response, a refusal, an unparseable body, an unknown verdict token, a candidate leg 1 refused a trace for, and a run with no verifier provider all record UNVERIFIABLE with the reason. No branch raises a severity or removes a finding, so a failing verdict pass can only leave the report as it was.
- A Python class-body hotspot renders as one — "Split oversized class body", remediated by moving cohesive members out — instead of borrowing the function vocabulary and telling a reader to extract the body of a class. This is the Python half of the #6082 whole-impl relabel, and it reads trusty-analyze's new `region_kind` rather than inferring from a missing function name.
- An `instructions.md` sitting beside the manifest is read and folded into the auditor/synthesis prompt, with nothing in the manifest declaring it. Everything an audit needs now travels with the engagement, the same principle `[inference]` applied to the provider — unzip an engagement directory, drop the file next to `manifest.toml`, and both `trusty-review report --manifest` and `trusty-audit render` pick it up, because render passes the manifest at its original path. Precedence is unchanged: `--instructions` wins, then `[report].instructions`, then the discovered file.
- The rendered Analyst Instructions note now names the file that extended the auditor prompt and states that the deterministic post-synthesis checks — reachability, topology grounding, the numeric guardrail — still ran. Custom instructions steer emphasis; they never switch a guard off, and one that asks for a claim the reachability check forbids is still rewritten or rejected.
- The Analyst Instructions section states how many synthesized claims this run's deterministic guards withheld, and points at the Synthesis Status list that names each one — previously the only signal was that list itself, at the end of the document.
- The report footnotes the two count classes an independent verifier re-derives and gets a different answer for: §8 states that its commit total counts commits as the forge reports them while a local `git log` on a squash-merged branch counts each squash once, and the Key Facts block names the counter behind its LoC and file-count rows and what that counter excludes relative to raw `git ls-files`.
- The investigation record carries two deterministic evidence sources it previously lacked: a test census enumerated over the whole tracked file list, so the `test coverage` dimension's counts no longer move with the byte budget (#6193); and bind/exposure facts read from the examined files' own source, so reachability classification has something to read besides markers in finding text — a network-facing component now classifies as evidenced instead of having its true reach claims withheld (#6191).
- `is_test_path` recognises a repository-root `tests/`, `test/`, `spec/` or `__tests__/` directory. The tracked list is repo-relative, so the directory checks — which all wanted a leading separator — matched none of them, and a Cargo crate whose tests live in the root `tests/` reported no test coverage.

### Fixed

- A finding claiming a file is "not present in diff" no longer blocks a PR that
  contains that file. A map call sees one chunk, so it cannot see the chunk that
  adds the file it calls missing; the claim is now checked against the whole
  changeset and the finding dropped when the changeset refutes it (#1873).
- Caller-supplied `referenced_code` is pinned to the map-reduce per-file prompts
  by an end-to-end test, and the branch restores and logs at error level any
  caller context that failed to reach it instead of reviewing without it
  (#2654).
- The verdict embedded in `review_body` is reconciled to the authoritative
  top-level verdict, so a merge gate reading `BLOCK` can no longer disagree with
  an `APPROVE` printed inside the review. On the map-reduce path both the grade
  and verdict reconciles now run after the shallow-review cap (#1902).
- `review_health` and `/health` report the `dry_run` a review invoked through
  that surface actually executes with, plus a `dry_run_reason` naming the gate —
  previously they reported the raw config flag while every review ran dry and
  posted nothing (#4254).
- `report_analyze_e2e::analyze_populates_complexity_and_findings` no longer
  fails when the temp path happens to spell a dropped diagnostic's code; the
  drop check reads the structured finding rather than the whole rendered page
  (#4387).
- The review body no longer carries the raw structured reviewer payload. Under forced structured output the model's response text IS the JSON object, and it reached `review_body` verbatim — an audit of 22 dry-run reviews found 19 posting `{"findings":[],"grade":"A",…}` as the entire PR comment. The unified path now renders the model's own summary and grade rationale instead; the map-reduce path was never affected (#4999).
- A finding citing a line beyond the cited file's last diffed line is now dropped. Citation integrity previously verified quoted content only, so a misattributed finding that quoted nothing substantial survived with its severity intact — the shape behind 5 of the 19 fabricated-or-partial findings in #4999's audit. The check reads the `@@` hunk headers, covers both sides of each hunk plus Stage-B-dropped hunks, and fails open when no header parses (#4999).
- **`trusty-review run` no longer posts live on the strength of an ambient env var alone.** `cmd_run` passed `TriggerDecision::None`, so whether a run published a GitHub comment depended entirely on whatever `PR_INTELLIGENCE_DRY_RUN` happened to be set to in the calling shell — with nothing at the call site indicating it would happen. That published an unintended `REQUEST_CHANGES` review to PR #4436 from a run meant only as a local pre-merge check. `run` now takes an explicit `--live` flag: passing it forces a live post (even over a stale `PR_INTELLIGENCE_DRY_RUN=true`), and omitting it forces a dry-run (even over a stale `PR_INTELLIGENCE_DRY_RUN=false`) — the env var no longer gates this command at all. `run` also prints the resolved posting mode to stderr before doing any work. See [#4460](https://github.com/bobmatnyc/trusty-tools/issues/4460).
- **A findings-parse failure now fails closed instead of rendering `Findings: none`.** A model that returned its `findings` array double-encoded — a JSON *string* holding the array — made the whole response fail to deserialize; the verdict-keyword scan then salvaged the verdict, discarded the evidence, and printed `APPROVE` with `Findings: none`, indistinguishable from a genuinely clean review. Two changes: the parser now decodes a double-encoded `findings` string a second time, so the findings usually survive; and when the structured payload still cannot be parsed, the keyword-scanned token feeds the fail-safe *reason* rather than becoming the verdict, so the run reports `UNKNOWN` plus a `Pipeline error:` line naming the lost findings. See [#4491](https://github.com/bobmatnyc/trusty-tools/issues/4491).
- `[confluence: "Page Title" — "excerpt"]` is now a recognised inline citation form. `BRACKET_CITATION_RE` matched four bracket forms and the two system prompts mandated the same four, but `duettoresearch/code-intelligence` emits a fifth — so a Confluence excerpt survived the pre-scan strip and was read as an ungrounded free-text code quote, tripping the fabrication check on prose the model had cited correctly. The regex and both prompt grammars now list all five. See #5022.
- The report's `Data gaps:` line no longer names a section the same report renders populated. Gaps are recorded as the polish pass walks the document, so dropping the Authorship & Key-Person Risk section's unsynthesised narrative paragraph recorded the enclosing heading as a gap — and nothing revisited that once the section's authorship table filled in with six authors, a bus factor and a trajectory. The gap list is now filtered against the finished body: a label matching a heading whose span carries content is dropped, while a genuinely collapsed section is still named. Same stale-emptiness class as #6029. See #6039.
- A review whose PR-metadata fetch produced no head SHA no longer posts. The dedup claim in `run_review` and the `complete()` call in `finalize_review` are both keyed on the head SHA, and `decide_action` never read it — so a failed `fetch_github_pr_meta` fell back to an empty SHA, continued, and could post live with the claim never taken and never completed; a retry after the same failure posted a duplicate, and the stranded-claim block could never engage. `run_review` now aborts before the diff is loaded when posting is reachable and the SHA is empty, and `finalize_review` downgrades such a run to log-only as a second guard for the webhook path. Same fail-open family as #5126/#5113/#5020. See #6062.
- A GREEN finding now carries its `file:line`, and only a GREEN carrying one is credited as a clean signal in Security Posture. Greens were admitted as bare titles with no file, no line and no quote — 0 of 23 in one report — and five of them were then cited as security strengths a reader had no way to check. One, "Raw SQL string interpolation via multi-line concatenation for PR upsert", describes a defect and was listed as a strength. The verification guardrail now holds for every band, and the bullet renders `title — \`file:line\`` without any elaboration.
- The Dependency Inventory's Locked column resolves the declared requirement against every locked version, not the first. A workspace lockfile carries several versions of one crate, and cargo writes them sorted, so first-wins reported the lowest: `base64` declared `0.22` rendered as `0.13.1`, `dashmap` declared `6` as `5.5.3`. The highest satisfying version wins; when none satisfies, the cell names every candidate and says so.
- The Code Quality table's Maintainability findings cell states the analyze-lane gap instead of a measured `0`. Maintainability findings come from trusty-analyze alone, but the count read `metrics.findings`, which the investigation pass also writes into — so a run whose index was rejected as stale asserted a measurement nobody made, on the page whose Gaps & Caveats said the lane was rejected.
- Values substituted into a markdown table cell have their `|` escaped. One Top Risks row said "the installer uses an unsigned curl|sh pattern", and that pipe split the cell so every column after it shifted.
- An evidence quote that stops mid-construct pulls in one more full line. Completing to end-of-line was not enough when the model's stop point already sat on a line boundary: `install.sh`'s security note wraps, and the quote ended "…download and review the script manually" while "All downloaded binaries are SHA-256 verified." sat on the next line — so the finding read as unmitigated remote code execution again.
- Key Facts' complexity row states the analyze lane's own remedy. It said "re-run with `--analyze`" for every case, including a stale-index rejection where §9 correctly said to re-index under a distinct id; a reader following Key Facts would re-run the same command and get the same rejection.
- The executive summary and the coverage section state one finding count with one label. They counted differently, so one report said "102 verified findings" and "79 verified evidence-backed findings" about the same set. Both now read `N verified finding(s) (R RED/AMBER evidence-backed, C clean signals)`.
- Dimension labels fold onto the canonical six at ingestion. The model wrote "error management" for two findings and "error handling" for twenty-three in one run, splitting one dimension's counts across two names. An analyst-focus dimension the crate does not enumerate survives verbatim.
- A dimension that produced verified findings is listed as covered. Coverage came from file selection alone, so a dimension no selected file matched by name read as not investigated while its findings sat in §5.
- The coverage section's per-dimension example is the dimension's own top-ranked evidence hit. It was the first file in overall rank order, so a file declared for one dimension that a path heuristic also matched became another dimension's example, carrying the wrong query as its stated reason.
- `report` reads `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` and
  `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES` as the tier below the manifest's
  `[report].investigate_max_*` keys. `trusty-audit` records the budget it wants in
  the manifest, but `tga audit` has already run this binary against that manifest
  by the time the key is written — so an audit declaring 240 files rendered its
  report under this crate's 40-file default and said nothing. An explicit
  `--investigate-max-files` and a declared manifest key both still win; a
  non-numeric, zero or negative variable reads as absent.
- A reachability rewrite can no longer ship ungrammatical debris ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - a replacement that opens with a determiner now takes the article in front of it, so `by a remote attacker` becomes `by any local process` rather than `by a any local process` — the last report's Security Posture lead shipped exactly that
  - every rewritten sentence is read for grammar debris before it may ship: two adjacent determiners, or a contrast whose two sides describe the same reachability, send the field down the reject-and-disclose path instead of out to a reader
- A corrected-wording disclosure now cites the §5.1/§5.2 number of the finding it is about ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the withheld lines already carried "section 5.1, RED finding 2" while the corrected-wording line beside them named its finding by title only; the grounding check now hands that finding to the reporter, which resolves it to the rendered number
- A withholding disclosure now names the finding whose field it is ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the reachability tier is read from the FILE, so the grounding check resolved it through that file's first loopback-scoped record and quoted that record's title; the last report's RED findings 2 and 3 both sit in `control_routes.rs`, so their two Synthesis Status lines shipped byte-identical, both naming finding 2
  - the check now takes the field's own finding title and prefers it over the shared record, for both the withheld and the corrected-wording line; report-level prose, which names no finding, still cites the one the subject-token match found for it
- The crate-topology table renders as a real markdown table again. `ct_row`'s BEGIN/END markers each sat on their own line, so every repetition emitted a leading and a trailing newline and the rows came out blank-line separated; that left the header and its `|---|---|---|` delimiter as a two-line table with no body, which the polish pass collapses and drops. The rendered report showed 15 orphan pipe-lines under no header at all.
- The topology table lists every crate. It used the `TABLE_ROW_CAP`-truncated list, and the sort puts `inbound == 0` crates last, so the cap dropped exactly the leaves — 15 of 30 in the dogfood run — under a summary line still stating 30 crates. The synthesis prompt still takes the capped list, which is a token budget.
- Analyze-derived `Component` values are repository-relative. trusty-analyze indexes a checkout by absolute path and reports findings by absolute path, so 21 places in one report carried the auditor's own filesystem layout (`/Users/…/repos/local/trusty-tools/crates/…`) into a document written for someone outside that machine — while the investigation pass cited repo-relative paths for the same files. A trailing `:line` suffix survives the strip; a component under a different root is left alone.
- A refactor suggestion with no function name is labelled an impl block, not a function. trusty-analyze reports `impl X { … }` as a region with `function_name: None`, and every downstream string then called it a function: the top four maintainability findings of the dogfood report were `impl HnswStore`, `impl Backend for JiraBackend`, `impl AzureDevOpsClient` and `impl ChatSessionStore`, each titled "Extract method", described with a `long_function` smell, and remediated by "Extract the body of 'this function' (lines N–M) into 2–3 smaller functions" — an action no reader can take on an impl block. The remediation now names splitting the impl across submodules, keeping the analyzer's line range; the `long_function` smell is relabelled `long_impl_block`; and the literal `'this function'` placeholder, which appeared 17 times, never renders — replaced by the real name when one was supplied, and the finding suppressed outright when neither a line range nor a rationale identifies the region.
- Findings that restate themselves no longer reach the report. Three of one report's 157 findings were the model re-reporting findings it had already made, sharing a shape no genuine finding has: the Evidence block quoted the finding's own label (`crates/…/hnsw_store.rs (extract method)`) instead of a line of source, and the Component named a topic (`trusty-common memory_core`) instead of a file. A quoted row is now dropped when its component names no file, when the quote spells its own file's path rather than quoting out of it, or when quote and title restate each other. A row with no evidence quote is untouched — that is an ordinary un-synthesized metrics row, which never claimed to carry evidence. `Cargo.toml` and `deny.toml` still read as real citations.
- A loopback-only endpoint is no longer written up as remotely exploitable. The executive summary, Top Risks row 1 and the Security Posture lead of the lap-4 dogfood report all called the trusty-mpm control plane "an unauthenticated remote-code-execution path", while the daemon binds loopback only, `guard_write_origin` sits router-wide, and the finding's own remediation says "before exposing them beyond localhost". The synthesis prompt now states each finding the report's own remediation or evidence text scopes to a loopback surface, and a post-check runs over every narrative field and top-risk description afterwards: a remote-reachability phrase in a sentence about one of those findings is rewritten to its local-process-reachable form, and a phrase the rewrite table does not cover fails the field closed with a note naming the finding it contradicts. A finding whose text scopes it to a network-reachable surface (`0.0.0.0`, internet-facing) vetoes the check for its own subject, so a genuinely remote endpoint still reads as remote.
- A crate nothing depends on is no longer called load-bearing. The same summary named trusty-mpm a load-bearing crate while the topology table two sections down showed it with 0 dependents. The prompt now carries the measured top quartile by dependent count as the authoritative list, and a claim naming any other declared crate as load-bearing or most depended on rejects the field.
- An LLM finding no longer credits a whole-impl complexity score to one method inside it. One finding read "The copy_all_from function has cyclomatic complexity 89" where the analyze data attributed that 89 to the enclosing impl block at `meta_ops.rs` lines 19–437. The claim is rescoped onto the region that carries it, keeping the number and the file; a claim in a shape this crate cannot rescope drops the finding with a logged reason rather than shipping the attribution.
- LLM findings that restate a measured hotspot are suppressed. Seven pairs sat side by side in the same AMBER list of one report — same file, a line inside the region, the region's own cyclomatic number quoted back — reading as two findings about one measurement. The deterministic entry is kept, since it carries the structured span, score and smell fields; each suppression is counted and logged with the region it duplicates.
- A synthesis narrative attaches to the finding it is about, not to whichever finding happens to share its title first. The merge keyed on title text alone, and the analyze daemon emits one canned title per remediation shape — the graded report carried 15 findings titled "Split oversized impl block". Both narratives that shared that title landed on the first of them, so the `hnsw_store.rs` row (cyclomatic 140, the worst impl hotspot in the estate) rendered the Jira backend's narrative and evidence, the hnsw narrative was never rendered at all, and the Jira row never received its own. Colliding titles are now separated by component identity, taken from the narrative's own component or, when the model wrote a topic phrase there, from the file its evidence names.
- A narrative that merely restates its own finding no longer deletes the measurement underneath it. The restatement filter dropped the whole merged row, so `crates/trusty-common/src/tickets/server.rs` (cyclomatic 118, grade F) vanished from a render that exited 0 listing 167 of the 168 AMBER findings the metrics measured. The junk narrative is still refused; the row now falls back to its raw metrics fields, which is how every un-synthesized hotspot already rendered. A narrative with no measurement behind it is still dropped outright.
- The reachability guard covers a finding's own narrative fields — description, business impact, remediation and cost/effort — not just the summaries and the top-risk rows. RED finding 3's business impact shipped "enabling remote/local code execution" two lines from a Security Posture paragraph the same guard had corrected to "not remotely". Evidence quotes stay excluded: they are verified verbatim against the file, so rewriting one would make the displayed quote diverge from the quote that was checked.
- `remote/local` is recognised as a remote-reachability claim. The hedged spelling matched no rewrite pattern, because the `/local` sits between the two words every pattern expected to be adjacent, so the sentence never triggered the check at all.
- A component carrying whitespace or parentheses is read as a topic phrase, not
  a file. `trusty-common (memory_core/store)` slipped through as a path because
  its parenthesised half carries a slash, and the narrative citing it rendered as
  a numbered AMBER finding whose entire evidence was the file's own path. (Refs
  #6082)
- A synthesis narrative the finding bands refuse is now recorded under Synthesis
  Status with the component that matched nothing, instead of disappearing without
  a line. (Refs #6082)
- The reachability guard now runs over the verified investigation's own prose,
  before that prose is copied onto the model and merged over the synthesis
  narrative. A RED finding's business impact reached the page stating "a remote
  code execution risk" about a loopback-bound daemon because only the synthesis
  copy was guarded and the merge discarded it. (Refs #6082)
- A finding whose own text says "local-socket" or "unix socket" is read as
  loopback-scoped, the same as one that says "localhost". (Refs #6082)
- The reachability guardrail now catches a claim of network reach, not only the word "remote". What makes a sentence its business is a vocabulary of reachability words rather than a table of known phrases, so a spelling it cannot rewrite is rejected and disclosed instead of shipped — the failure that let "an unauthenticated actor on the network or host" through for a daemon bound to 127.0.0.1.
- A Security Posture paragraph claiming no clean signal was credited is dropped when the section credits some, and the synthesis prompt now states the measured count so the model has no reason to write the claim.
- The template's closing signature moves to the true end of the report. It used to sign off three sections early, above Synthesis Status, Dependency Inventory and Investigation Coverage.
- Synthesis Status names the section 5.2 finding a withheld narrative belonged to and says the narrative could not be verified against the collected data, replacing wording that described the matcher rather than the report.
- Investigation Coverage states how the verified-finding count adds up to what section 5.2 renders: verified findings plus the complexity findings trusty-analyze measures mechanically.
- A truncated or failed investigation batch is now disclosed in Gaps & Caveats, naming the batches and the files they carried. It was disclosed only in the executive summary and the coverage appendix.
- A finding's reachability now follows the file it sits in, not the words its own text happens to use ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - every finding on a file some finding scopes to localhost inherits that scope
  - a beyond-the-host reach claim about a file the collected data says nothing about is withheld and disclosed, never rewritten
  - a finding is judged by its own component, so an unrelated finding's title words can no longer empty its business impact
- Every Synthesis Status line is now written for the reader ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the bare `synthesis: available` banner is gone, and the block is omitted when there is nothing to disclose
  - a line about a finding cites the §5.1/§5.2 number the reader can look up instead of naming it by title alone
  - citations for RED findings say section 5.1; they used to say 5.2
- An unevidenced component's reach claim is now caught by vocabulary, not by a table of known phrases ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - `remote`, `network` and `internet` are read as tokens, so a hyphenated compound (`remote-execution`) and a morphological variant (`remotely`, `network-reachable`) are examined; the last report shipped "a critical remote-execution and privilege risk" because no rewrite pattern matched it
  - only CLAIM-shaped uses are withheld — the word must sit within three tokens of an access, attack, execution or exposure word — so "a transient network error" and "network and streaming logic" keep their field
- A Synthesis Status disclosure about a report-level paragraph now cites the §5.1/§5.2 number of the finding it is about ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the executive, code-quality, security and authorship summaries belong to no single finding, so their rejections quoted a finding title with no number; the grounding check now hands that finding to the reporter, which resolves it to the rendered number
- Report synthesis no longer fails outright on a large finding set. The request's output-token ceiling was a hardcoded 3072 that ignored `[models.reviewer].max_tokens`, and the single truncation retry shrank the top-risks table from five rows to three while leaving ten per-finding elaborations — the dominant output cost — untouched at the same ceiling, so it could not converge. A 44- and a 45-finding investigation both truncated twice and exited with no report; 26 findings rendered clean. The retry is now a three-rung ladder that raises the budget and shrinks the ask together: full ask at the configured ceiling, then a concise ask at double it, then a narrative-only ask at quadruple it with no elaboration array in the schema at all. No finding is dropped — every RED/AMBER finding still renders from the deterministic composition and from the investigation's own verified prose.
- `[models.reviewer].max_tokens` now reaches the synthesis request instead of being overridden by a constant.
- A model id whose shape names one provider no longer runs on another. An
  unprefixed OpenRouter slug (`anthropic/claude-opus-4.8`) used to take whatever
  `provider` the path defaulted to; on the #6093 mitigation run that was
  Bedrock, which rejected the id, and the render that completed used Bedrock's
  `us.anthropic.claude-sonnet-4-6` default instead. `resolve_provider_and_model`
  now routes an unambiguous shape to its own provider — slash-form slugs to
  OpenRouter, `us.anthropic.*` / `anthropic.*` ids to Bedrock,
  `accounts/fireworks/models/*` to Fireworks — and returns
  `LlmError::Validation` naming both the id and the provider when the two cannot
  be reconciled. A genuinely ambiguous bare id still uses the configured
  default, and an explicit routing prefix is overruled only by a shape that
  belongs to exactly one provider's catalogue — never by the dotted
  `vendor.model` guess, so `openrouter/anthropic.claude-x` keeps working.
  `resolve_provider_and_model` is now fallible; `build_provider`
  propagates the error, and `trusty-review report` raises it in its preflight,
  before a sweep spends minutes on a render that would use the wrong model.
- A per-request model override now reaches the provider. `OpenRouterProvider`
  and `FireworksProvider` serialised the id they were constructed with and
  dropped `LlmRequest.model`, so a request naming a different model ran on the
  constructor's model and the response echoed the wrong id with nothing
  reporting the substitution; `BedrockProvider` already read `req.model`. All
  three now resolve the same way through `LlmRequest::effective_model`, which
  returns the request's id and falls back to the constructor's only when the
  request names none. `OpenRouterProvider::with_base_url` is new — the
  endpoint was a hardcoded constant, so nothing could assert on the bytes the
  provider actually sends; the regression tests now read the model out of a
  mock server's received body.
- The numeric guardrail now admits scaled-unit restatements of a large integer, so "1.55 million lines" for a measured 1,553,771 verifies instead of vetoing the whole field. It also admits the figures the report computes at render time and prints in its own sections (the investigation coverage percentage, the authorship trajectory average), which the synthesis prompt already quotes verbatim.
- Code Quality & Architecture reads LoC and primary tech through the same source precedence Key Facts uses, so a `--analyze` run — whose fetch leaves `loc`/`counts` to the scanner by design — no longer prints "not stated in source data" beside a §4.1 that renders the scan's figures.
- AMBER findings render their component path, so a complexity finding no longer reads "Extract the body of 'this function' (lines 23-512)" with no file named.
- Analyze data whose component paths fall outside the audited checkout is rejected as stale-index evidence with a named gap, instead of being stamped measured. An index is addressed by directory basename, so a second checkout of the same repository answers for the audited one.
- Every GREEN finding renders as its own topic line. The templates carried three fixed slots, so a run with 21 GREEN findings dropped 18 of them.
- Security Posture counts only the security dimension's findings, credits that dimension's clean signals, states its actual provenance (verified LLM investigation findings, code-hygiene scope), and prints one table rather than one per row.
- Executive-summary jump-list anchors match GitHub's rule (punctuation deleted, spaces mapped to dashes), so a heading containing `&` no longer links to an anchor that does not exist.
- An empty Dependency Inventory renders as a named gap naming what was or was not examined, never as "no manifest-declared dependencies were found". `[workspace.dependencies]` is now inventoried, which is where a cargo workspace root declares its dependencies.
- Performance & Scalability points at section 5 when the investigation raised scalability findings, rather than reading as though neither was assessed.
- A verified evidence quote is completed to the end of the line it ends on, so a mitigating clause on that same line cannot be cut away from the finding it qualifies.
- `report::derive_index_id` now returns the per-checkout id
  `trusty_common::derive_checkout_index_id` derives (slugified basename plus 8
  hex digits over the canonical path) rather than the bare basename. The
  renderer and the audit that indexed the tree run as separate processes sharing
  only the manifest's checkout path, so both deriving the basename meant a
  machine holding two checkouts of one repository served whichever registered
  first — which is how a report presented another tree's complexity as a
  measurement of the audited one. The out-of-tree component check in
  `report::analyze_scope` stays as the backstop for an index registered under
  the old scheme.
- A doc comment on `parse_entry_node` in `trace_client.rs` tried to escape backticks inside a markdown code span with `\``, but backslash does not escape inside a CommonMark code span — the span closed early and `[ENTRY]` fell outside it, where rustdoc read it as an unresolved intra-doc link. The span now uses a longer backtick-run delimiter (` `` ` instead of `` ` ``) so the inner backticks need no escaping. Caught by the pre-publish rustdoc broken-link gate ahead of the 0.24.0 release.

### Changed

- `store::redb_error_is_incompatible_format` now delegates to
  `trusty_common::redb_open::is_incompatible_format` instead of carrying its own
  copy of the four-arm `match` (#5063). Same verdict for every input; the dedup
  store's recovery policy is unchanged and stays in `store::dedup_open`, because
  it must serialise the rename-aside behind the sidecar recovery lock (#5064).
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and this crate's own `mcp` feature forwards to `dep:trusty-mcp` rather than `trusty-common/mcp`. The feature is still on by default, so a default build is unaffected. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
- The Security Posture prompt no longer calls the findings "lint-graded". They are an LLM's readings of source files with every quote mechanically verified — which the disclaimer directly above the paragraph already said, so the paragraph contradicted it.
- The investigation prompt asks every finding, GREEN included, for a `file` and a verbatim `evidence_quote`, tells the model a GREEN title must name a strength rather than a weakness, and to copy each dimension exactly as the checklist spells it.
- A verified GREEN finding keeps the evidence quote its citation was verified from; only its prose is dropped. Blanking the quote is what made a green citation uncheckable: "Manifest crate reads OS-level environment for API token resolution parity" cited `trusty-agents-common/Cargo.toml:58`, which is `fd-lock = { workspace = true }` under a file-locking comment, and neither the page nor the JSON twin retained anything that could show the mismatch. The rendered bullet still shows only the topic and its `file:line` — the no-green-analysis rule bans elaboration, not evidence — and a finding with no surviving quote now renders with no `file:line` at all rather than a pointer a reader cannot check.
- The Security Posture preamble scopes its verification claim to the RED and AMBER findings the table counts, and states that a GREEN clean signal carries a `file:line` only when a verified quote backs it. It previously asserted that every finding's evidence quote was mechanically verified while all 76 GREEN findings carried an empty quote.
- AMBER findings render their business impact. `business_impact` was present for all 138 red/amber findings the investigation produced and rendered only for the 3 REDs, because the amber block in both bundled templates had no slot for it.
- A repository whose manifest declares no attributed evidence now says so in its coverage section — "evidence discovery: path-name heuristics only" — instead of rendering identically to a searched run. The cause stays in Gaps & Caveats, where the manifest's writer records it.
- The MCP `review_pr` / `review_diff` tools fail a `reviewer_model` override
  they cannot honour. #1357 made this case detectable — it ran the review on the
  startup provider and reported a `reviewer_model_fallback` string — but that is
  still a verdict from a model the caller did not ask for. The tool call now
  returns an error naming the requested model, and the `reviewer_model_fallback`
  envelope and payload field is removed, since nothing can set it any more.
- A model id whose shape contradicts the provider pinned for the call now
  resolves instead of failing (owner ruling 2026-08-21: "Models selection should
  be robust. We shouldn't fail on naming/id issue. The report should include
  which models are used."). Where the pinned provider publishes the same model,
  the id is translated into that provider's own **verified** spelling —
  `bedrock/anthropic/claude-sonnet-4.6` runs `us.anthropic.claude-sonnet-4-6`;
  where it does not, the call routes to the provider whose catalogue the id
  belongs to. The translation reads the verified `ModelTier` catalogue and never
  derives an id by string surgery, so an unmappable direction (Bedrock publishes
  no Opus 4.8 profile) resolves by routing rather than by inventing an id. The
  #6114 guarantee is unchanged in substance and moves from refusal to
  attribution: every adjustment is logged and printed in the report as
  `requested → ran`. `LlmError::Validation` remains for the one case with no
  reasonable resolution — an id belonging to a provider this build cannot call,
  with no verified equivalent in one it can. The MCP `review_pr` / `review_diff`
  `reviewer_model` override follows the same rule.
- BREAKING: `RepositoryEntry` (report::manifest) and `RepositoryReport`
  (report::model) each gain a public `crate_topology` field. Both are
  exhaustively constructible through the public API, so any external struct
  literal over either one stops compiling and must add the field (`None` keeps
  the previous behavior). This is why the crate goes to 0.23.0 rather than a
  patch release — for a `0.y.z` crate the breaking bump is the MINOR position.
- `RepoInvestigation` gains a `traces` field. For a `0.y.z` crate that is a MINOR bump, so this release is 0.24.0.
- A present-but-unreadable `instructions.md` beside the manifest now fails the report run with `InstructionsUnreadable` instead of being ignored. Fail-open covers an input that is absent, never one the engagement author put there and this process could not honour — bad permissions, a dangling symlink, or non-UTF-8 bytes would otherwise render a report the author believes carries their instructions. An absent file is still the normal case and changes nothing; an empty one is still a warning.
- **`trusty-review serve` binds a Unix socket instead of a TCP port.** The daemon serves `<data dir>/trusty-review.sock` and answers three JSON-RPC methods — `review.health`, `review.status`, `review.run` — replacing `GET /health`, `GET /status` and `POST /review`. `--socket` overrides the path; `--port` and `--bind` no longer do anything (see below). `--stdio` (MCP) is unchanged. The `http_addr` discovery file is no longer written: the path is derived, so the daemon and its consumers cannot disagree about it. `DEFAULT_PORT` (7891) is removed, and 7891 is released. `SelfOrigins` / `with_guarded_middleware` are deliberately not ported — browser-CSRF machinery has no meaning on a socket; the boundary is the `0700` directory, the `0600` socket, and a peer-uid check on every connection (ADR-0032, [#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- **`trusty-review port` is now `trusty-review socket`.** It prints the socket path and whether anything is answering on it, and exits non-zero when nothing is — so `$(trusty-review socket)` still fails cleanly. `--addr` is gone with the address it printed; `--json` now emits `{"socket":"…","serving":…}` ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- **The launchd plist passes `--socket <path>` instead of `--port 7891`, and an un-reinstalled one still works.** Upgrading the binary does not rewrite the plist — only `trusty-review service install` does — so `serve` accepts `--port` and `--bind` as no-ops, hidden from `--help`, and warns at every start naming that command. Without the shim clap exits 2 on an unexpected argument and `KeepAlive::Always` turns that into a crash loop every 10 seconds, with only a usage message in the launchd log to explain it. Rerun `trusty-review service install` to clear the warning; the flags will be dropped once no unit passes them ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- The `http-server` feature no longer pulls `axum`, `tower` or `tower-http`. The name is kept so consumers' manifests are unaffected ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- `report --analyze` dials trusty-analyze's Unix socket (#6287, ADR-0032). Its
  per-endpoint budgets, fail-open contract, and Gaps & Caveats vocabulary are
  unchanged: `classify_failure` reads JSON-RPC code `-32005` where it read HTTP
  504, so a daemon-side deadline still prints "did not answer within the time
  allowed" rather than "could not be reached".
- The #6038 keep-alive retry is removed. It recovered a pooled HTTP/1.1
  connection the daemon closed mid-request; the socket client dials per call, so
  there is no pooled connection to lose.
- The context gate's skip message no longer names an analyze daemon address —
  no path on that gate contacts one.
- `report --analyze` starts trusty-analyze itself rather than requiring a
  resident daemon, and restarts it if it idles out mid-report: a request that
  finds the socket gone respawns the server and retries exactly once. A timeout
  does not retry — the server answered and is working. A start that fails is
  reported through the "falling back to scan" line on stderr plus an
  unassessed-dimension entry under Gaps & Caveats, never silently degraded to a
  clean-looking report (#6350).

### Removed

- The `serve` daemon, the `socket` subcommand and the `service` launchd wrapper
  are gone — trusty-review runs per invocation, with no resident process and no
  LaunchAgent (#6290, ADR-0032).
- `trusty-review run --json` prints the same `ReviewResult` object the retired
  `review.run` method returned, field for field.
- A failed run now prints that JSON with the reason in `error` AND exits
  non-zero; it previously exited 0 on a provider outage.
- The MCP stdio service moved to `trusty-review mcp`. `serve --stdio` is kept as
  an alias, so no existing `.mcp.json` needs editing.
- The `service::rpc` module is gone from the library: `serve`,
  `serve_with_shutdown`, `build_router`, `socket_path`, the `METHOD_*` and
  `MAX_FRAME_BYTES` constants, and `NoParams` are all removed. This is the
  break behind the 0.29.0 bump.

### Documentation

- `build_review_state`'s doc describes the `SubprocessAnalyzeClient` it
  actually builds — spawning the `trusty-analyze` binary per call, no daemon —
  rather than the loopback `DEFAULT_ANALYZER_URL` default that #6287 deleted.
  The broken link failed Gate 1 of the pre-publish workflow.

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
