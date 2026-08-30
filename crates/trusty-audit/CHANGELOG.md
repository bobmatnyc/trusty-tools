# Changelog — trusty-audit

All notable changes to trusty-audit are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

Entries are assembled at release time from `changelog.d/` fragments — never
edit this file by hand (see
[docs/reference/changelog-fragments.md](../../docs/reference/changelog-fragments.md)).

---

## [0.12.0] — 2026-08-29

### Breaking

- `grounding::Tools::search_url: String` is now `grounding::Tools::search_socket: PathBuf` — the trusty-search half of the pair names a Unix socket rather than a base URL.
- `grounding::evidence::HttpSearch` is now `grounding::evidence::SocketSearch`, and its constructor takes a socket path and is infallible: there is no HTTP client left to fail to build.
- `grounding::daemons::search_base_url` is removed. `grounding::daemons::search_socket` replaces it, forwarding to the new `grounding::search_rpc::search_socket`.

### Fixed

- Resolved a broken rustdoc intra-doc link in `grounding::search_rpc` (a bare
  `[`Display`]` now points at `std::fmt::Display`), unblocking the rustdoc
  intra-doc-link publish gate. No behavior change (#6285).

### Changed

- The three legs that reach `trusty-search` — the readiness probe, the index-root backstop, and the per-dimension evidence queries — now speak framed JSON-RPC over the daemon's Unix socket instead of loopback HTTP (`search.health`, `search.index.status`, `search.query`). The socket path is derived from `trusty_common::daemon_socket_path("trusty-search")`, the same call the daemon binds, so there is no address to discover and no `http_addr` file for a stale write to contradict. `TRUSTY_SEARCH_SOCKET` pins it explicitly for a rig that started its own daemon.
- A daemon that answers and refuses stays distinguishable from a daemon that is not there. Both remain fail-open at every leg, as before, but an error frame can no longer read as an empty result — a refused query is reported with the daemon's own code and message rather than as "no evidence found".

## [0.11.1] — 2026-08-28

### Fixed

- A run's child logs go into `logs/<run>/` — one directory per launch — instead
  of straight into `logs/<NN>-<repo>.log`, which `File::create` truncated. Before
  this, starting a second audit destroyed the record of the first before it had
  produced anything to replace it, so an operator comparing a failing run against
  the one that worked had nothing left to compare against; an external engagement
  reported the same loss independently. The directory is named for the local time
  and the process id and is created exclusively, so two launches in one second get
  their own; a repository carried over by the checkpoint keeps the log path from
  the run that actually audited it, and the run index links whichever that is. The
  twenty most recent runs are kept and older directories are removed as each run
  starts, so persistence does not become unbounded growth — a run that dies never
  reaches an end, so pruning at the end would not be retention (#6137)
- A repository's output directory carries an `audit-complete.toml` marker,
  written last and only for a repository the run got all the way through, and a
  re-run re-collects any recorded output that does not have one. `manifest.toml`
  was the only evidence a directory held, and it is written by the `tga audit`
  child before the grounding pass and the inference stamp edit it — so a run
  killed in between left real collection data and a report that was never
  finished, which `verify_output` accepts and a re-run carried over as complete.
  Completion was recorded only in `state/run-progress.toml`, one document for the
  whole sweep, so nothing about the directory itself said which it was; the
  marker makes that answer travel with the data, for a directory copied,
  inspected or re-rendered on its own. The re-collection is announced with its
  reason rather than being silent, and a record written before the marker existed
  is re-collected once — the same direction `RunProgress::complete` takes for the
  same reason (#6141)
- The `tga` stub the run tests install writes its invocation log to an absolute
  path under the test's work directory, and `invocations()` panics when that log
  is absent instead of reading it as an empty list. The stub used to append to
  `invocations.txt` relative to the child's own working directory — which only
  `run::child::spawn_tga` sets — so the same script installed behind
  `trusty-analyze` and `trusty-review` wrote into whatever directory the test
  binary started in, and every `assert_eq!(invocations(&work).len(), n)` then
  compared zero against a count nothing had recorded. Running the suite from the
  crate directory put the log in the checkout, which is how 49 blank lines of it
  were committed and then ignored (`f3ec62545`); that `.gitignore` entry is
  removed with the leak (#6293)

### Changed

- `grounding`'s analyze guard and hotspot fetch dial trusty-analyze's Unix
  socket instead of port 7879 (#6287, ADR-0032). As in `tga`, the health verdict
  is explicit — only `status: "ok"` passes — because a JSON-RPC health call
  answers with a result frame whether or not trusty-search is reachable, and a
  degraded daemon serves an empty hotspot list that reads as "nothing complex".
- `TRUSTY_ANALYZE_URL` becomes `TRUSTY_ANALYZE_SOCKET`. The value it carries is
  a filesystem path now, and a variable still saying URL would leave an operator
  setting `http://…` and getting a dial failure naming a socket they never
  configured.

### Documentation

- `Tools::pinned`'s doc links `daemons::analyze_socket` instead of the
  `daemons::analyze_base_url` that #6287 removed, and says the resolved pair is
  a search URL plus an analyze socket path. The broken link failed Gate 1 of
  the pre-publish workflow.

## [0.10.0] — 2026-08-25

### Added

- `trusty-audit init` writes the first `engagement.toml` with no terminal
  involved, reading the key from `OPENROUTER_API_KEY`. Only the interactive
  cold start ever wrote that file, so the README's documented sequence failed at
  `add repo` with `NoEngagementConfig` for every scripted and CI caller, whose
  workaround was faking a pty with `script -q /dev/null`. Re-running it over an
  existing engagement is a no-op, so it can sit at the top of a script.

### Fixed

- `render` now refuses when the engagement's own `tools/trusty-review` is at a
  different version from the engagement's `trusty-review` pin, naming both. It
  used to render at whatever that copy happened to be and exit 0. `install`,
  `run` and `guided` all reconcile pins; `render` never did, and its default
  renderer IS that engagement-local copy — so bumping a pin and re-rendering
  silently produced the report at the old version, with the only trace a version
  in the report metadata, read after the report had been believed.
- The reconciliation covers only the engagement's copy. `--review-bin` stays the
  escape hatch and is never reconciled, and a `PATH`-resolved renderer keeps the
  disclosure it already carried rather than gaining a refusal. A re-render where
  no `engagement.toml` loads pins nothing and is unaffected.
- The `gh` login now reaches the child's git transport. The sweep already read
  `gh auth token` once and handed it over as `TRUSTY_AUDIT_GITHUB_TOKEN`, which
  only tga's `github:` config section references; the fetch that runs before
  every collection reads `GITHUB_TOKEN`. A recipient logged in with `gh` and
  nothing else had every fetch fail and got a header-only `pr-metrics.csv` per
  repository, over a usable credential this process was holding. An
  operator-exported token is never replaced — the forward happens only when the
  `gh` login is the sole source.
- A sweep with no non-interactive git credential at all refuses before any child
  runs, naming how many repositories would have been fetched and every way to
  supply one (`gh auth login`, `GITHUB_TOKEN`/`GH_TOKEN`, an SSH key,
  `ssh-agent`). It fires only when the selected checkouts provably name a
  `github.com` remote, so an engagement over paths on disk still runs. Before
  this the run reported success having collected no pull-request data at all,
  and said so only in one repeated line inside each repository's own manifest.
- A repository that failed now leaves a trace in the return package. Each one
  ships its child's own log as `failures/<stem>.log`, copied through the same
  symlink, hardlink and credential guards every other member goes through, and a
  generated `failures/index.md` names the repository, what went wrong, how long
  it ran, and the gaps it stated before it stopped. A failure with no log says
  so rather than being absent. Before this a failed target left nothing at all —
  "failed" and "never attempted" were the same observation from the outside, and
  diagnosing either meant re-running the sweep on the machine that had it.
- The generated members are scanned for the engagement's credentials before they
  are written, rather than trusted because this crate wrote them. The failure
  record quotes reason strings, and a reason can carry text a child produced.
- An engagement config key this version does not act on is declared instead of
  discarded. The keys are captured at parse time, named in `package.toml` as
  `config_not_acted_on` (always emitted, so an empty array is a positive claim
  of full coverage), and stated in the return package's README and in the
  `excluded` lines a front end prints. Before this, a config requesting extra
  export families — per-commit tables, CODEOWNERS metadata, branch-protection
  metadata — produced output byte-identical to a run that requested none, with
  no skip or unsupported marker anywhere, so the operator was left inferring
  silence as "the config did nothing".
- The limit is stated rather than implied: this covers TOP-LEVEL keys. A stray
  key written after a table header belongs to that table in TOML and is still
  absorbed by that table's own type.
- The investigation budget an engagement declares now reaches the process that
  samples the files. `engagement.toml` gains a `[report]` section carrying
  `investigate_max_files` / `investigate_max_bytes`; the sweep resolves the
  budget ONCE from it (declared key > environment override > default, per key,
  with an absent byte budget derived from the file count) and hands that one
  value both to every `tga audit` child and to the grounding pass that records
  it. Before this the child read the environment inside its own spawn while the
  manifest was written from a second, later resolution — so a run could hand
  back a manifest declaring 240 files beside a report whose coverage section
  claimed 40.

## [0.9.0] — 2026-08-22

### Added

- A grounding leg measures the crate topology of any audited repository whose
  root `Cargo.toml` declares a `[workspace]`, and writes it into that
  repository's `manifest.toml` as a `crate_topology` table: member count, each
  member's direct internal dependencies and how many members depend on it, the
  total edge count, and any dependency cycle over those edges. It runs
  `cargo metadata --no-deps --format-version 1` — no dependency resolution, no
  network — and reads three keys out of the result, so it adds no dependency to
  the crate. `trusty-review` renders those numbers as a deterministic table in
  Code Quality & Architecture and states them to the architecture paragraph, so
  that paragraph comments on the workspace's real shape instead of inferring one
  from complexity buckets and a language list.
- Dev-dependencies are deliberately not counted as architecture edges: cargo
  permits a dev-dependency cycle, so counting them would report a routine
  test-only arrangement as a cycle. A cycle over the edges that remain is one
  cargo itself rejects, which the report states as such.
- A repository that is not a Cargo workspace is a declared skip, not a gap — it
  has no crate topology to miss, and its report is unchanged. A repository that
  IS a workspace whose metadata could not be read is a named gap, naming the
  repository and what its report therefore will not carry.

### Fixed

- Evidence discovery scores hits against a share of their own query's best hit
  instead of an absolute 0.15 floor. `POST /indexes/{id}/search` fuses its lanes
  with Reciprocal Rank Fusion, so a hit's score is `Σ weight / (60 + rank)` and
  can never exceed `1/61 ≈ 0.0164` — an order of magnitude below the floor. Every
  hit of every query was therefore dropped on every run, and the discovery leg
  produced nothing whatever the index held: the 2026-08-22 dogfood audit searched
  an 85 840-chunk index and wrote zero search-derived evidence rows. The relative
  floor drops the same filler beside a strong hit and can no longer empty a
  result set, because the best hit always clears it.
- A search leg that matched nothing now states its gap even when the repository
  has a build manifest. The deterministic `Cargo.toml` enumeration ran before the
  gap check read the dimension list, so it created a dimension on a run where
  search answered nothing at all — and the manifest then declared
  `attributed_only = true`, telling `trusty-review` to decline its path-name
  top-up over a sample with no search evidence in it. Both the gap line and
  `attributed_only` are now decided by the search result alone.
- The investigation budget reaches the renderer through the `tga audit` child's
  environment as well as through the manifest. `tga audit` renders from the
  manifest in the same process that wrote it, and this crate's grounding pass
  edits that file only after the child exits — so the recorded budget reached a
  re-render and never the sweep's own report. The 2026-08-22 run shipped a
  manifest declaring `investigate_max_files = 240` over an investigation that
  recorded `max_files: 40`. An explicit flag and a declared manifest key both
  still win.
- A ranking written into a manifest that has already been rendered from now says
  so on the console, and names `taudit render` as the recovery. That degradation
  previously had no line anywhere: the same run shipped 37 ranked paths beside an
  investigation whose every examined file read "path-name heuristic".

### Changed

- `grounding::quality::MIN_EVIDENCE_SCORE` is replaced by `MIN_EVIDENCE_SHARE`,
  and `is_evidence` takes the floor its query earned as a second argument.
  `evidence_floor` derives that floor from the query's best hit.

## [0.8.1] — 2026-08-22

### Fixed

- The grounding pass indexes each audited checkout under an id derived from its
  canonical path (`trusty_common::derive_checkout_index_id`) instead of the
  checkout basename, so a machine already serving an index of the same name for
  a different tree no longer collides. The confirmation run of 2026-08-21 hit
  exactly that: the engagement clone at `…/repos/local/trusty-tools` derived
  `trusty-tools`, the daemon was serving `~/Projects/trusty-tools` under it, and
  the root-mismatch guard correctly refused — which cost the report both its
  evidence discovery and its complexity hotspots. That guard stays, as the
  backstop for an index registered under the old scheme or a tree that has since
  moved. Existing indexes registered under a bare basename are not renamed;
  the first audit of a checkout re-indexes it under the new id.
- The investigation budget reaches `[report].investigate_max_files` /
  `investigate_max_bytes` even when both grounding legs degrade. The budget is
  configuration, not evidence, and it used to be written only alongside a
  non-empty ranking — so a run that lost its index also lost its 240-file budget
  and trusty-review fell back to its own 40-file default, compounding one
  failure into two.

## [0.8.0] — 2026-08-22

### Added

- The manifest declares `[report].attributed_only = true` when the search leg produced per-dimension evidence. trusty-review then reads only the declared ranking instead of topping the file budget up with path-name heuristics. On every degraded path — no index, a dead daemon, a complexity-only ranking — the key is not written and the existing gap line names why.
- A minimum relevance floor for search evidence. A hybrid search returns its top-N whether or not it found anything, so an empty-handed query returned filler rows that the ranking then promoted as headline evidence — the graded self-audit's "authentication & secrets" dimension led with a call-graph test file at score 0.02 while the real middleware went unread. Hits below the floor are dropped.
- Test-file demotion. For every dimension except "test coverage", a path matching the repo's test classification (`tests/`, `_test.rs`, `tests.rs`, `.test.ts`, `_test.go`, `benches/`, …) sorts behind every production file, so a test never outranks the file it tests. Demotion, not exclusion: a repository whose only auth-adjacent file is a test still surfaces it, last. Under "test coverage" test files stay first-class.
- The dependencies dimension now leads with the build manifests actually present in the tree — root and member `Cargo.toml`, `deny.toml`, `package.json`, `go.mod`, `pyproject.toml` and friends — enumerated by a bounded directory walk that skips `node_modules/`, `target/`, and `vendor/`. Finding a manifest needs no index and no LLM, and the previous semantic-only answer produced a false "no manifest-declared dependencies" claim on a 134-dependency workspace. Semantic hits keep their place behind the manifests; a tree with no build file is left alone rather than given an invented entry.
- An index-root guard. The trusty-search index id is the checkout BASENAME, so two checkouts of one repository collide on a single id — a sweep measured its complexity hotspots against a different clone, on a different branch and commit, and stamped them "measured". Grounding now compares the served index's `root_path` to the audited checkout, resolving symlinks and `..`. A mismatch is the named gap `complexity data unavailable: index root mismatch`, naming both roots, and both index-reading legs degrade rather than importing another checkout's measurements. A daemon that will not answer is not a refusal — that failure already has a gap of its own.
- The audit now discovers evidence by ASKING the repository's trusty-search index, not only by measuring complexity: one query set per due-diligence dimension (credential handling, swallowed errors, lock/cache consistency, scaling limits, tests), plus queries derived from the engagement's own `instructions` brief. The hits are interleaved round-robin with the complexity hotspots, so the report's inference budget is spent across the dimensions rather than down whichever one complexity concentrates in.
- Each `inspect_priority` entry the audit writes now records WHY that file is ranked — the dimension it is evidence for and the query (with its score) that found it — which the rendered Investigation Coverage section states per dimension.
- The audit asks for a wider investigation budget through the manifest: `investigate_max_files = 120` / `investigate_max_bytes = 1200 KiB`, written per key and only where the manifest declares none, and overridable per machine with `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` / `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES`.
- The sweep writes the run's inference identity — provider plus the three role
  model ids — into each repository's `manifest.toml` as an `[inference]` table,
  beside the ranking it already writes there. `trusty-review` resolves from that
  section ahead of the host's own config, so `trusty-audit render` on a
  recipient's machine reproduces the provider the engagement ran on instead of
  inheriting whatever that machine is pinned to (owner ruling 2026-08-21: "In
  audit mode, trusty review should use the same provider as audit to make it
  portable. From the manifest."). The same values still go to the child as
  `TRUSTY_REVIEW_*` variables; the manifest is authoritative when present. The
  section carries identity and never a credential — the key stays in the
  environment. A manifest that cannot be written is a named gap, not a failed
  repository, exactly as a ranking that cannot be written is.
- `index.md` gains an Inference section stating which provider and models
  produced the reports in that directory, and which layer selected them. A
  re-render reads it from the manifests it renders, since those outrank anything
  the run itself injects.

### Fixed

- A local-path target now audits under the `owner/repo` its checkout's `origin` remote names on github.com, instead of the synthetic `local/<name>` that made every work-item lookup 404 (3152 of 3152 on the trusty-tools self-audit) and stopped the audit before it could package. The on-disk identity is unchanged — a checkout is still acquired under `repos/local/<name>`; only the GitHub-issue identity handed to tga is resolved from the remote. A checkout with no remote, or one that names a non-GitHub host, generates a `github:` section declaring the work-item leg absent with the reason, so the run completes, packages, and names the gap in its report. Port-qualified remotes (`ssh://git@github.com:22/owner/repo`, `https://github.com:443/owner/repo`) resolve to the same slug their portless forms do, and a `git` that cannot be run is reported as a remote that could not be READ rather than as one that names no GitHub repository.
- The investigation byte budget now derives from the file budget at 10 KiB per
  file unless the environment or the manifest declares one. `trusty-review`
  stops selecting at whichever of the two caps binds first, so raising
  `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` alone read fewer files than it asked for
  and reported nothing about it — a 120-file lap read 76 files because the fixed
  1.2 MiB cap bound first. An explicit `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES`, or
  a declared `[report].investigate_max_bytes`, still wins outright.
- The default investigation budget is 240 files and 2.4 MiB per repository, up
  from 120 files and 1.2 MiB: 120 sampled about 1.8% of a workspace-sized
  repository, below the coverage a due-diligence report is expected to reach.
  The evidence caps follow it — 240 ranked paths and 34 files per dimension.

### Changed

- A search hit that the knowledge graph reached, rather than the query text, now says so in its reason: `via knowledge-graph expansion`. trusty-search labels the lane itself (`match_reason: "hybrid+kg"`); the evidence leg now reads that label instead of discarding it, so the report can distinguish "this file mentions credentials" from "this file is one call hop from the credential handler".
- The evidence queries pin `expand_graph: true` explicitly. It is already the daemon's default, so behaviour is unchanged — the pin keeps a future default flip from silently dropping relationship evidence from every audit.
- The evidence caps now scale with the investigation budget. `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` used to raise how many files trusty-review read while the priority list stayed 60 and each due-diligence dimension stayed 8, so everything past the 60th file reached the investigation with no dimension and no reason. The ranked list now tracks the budget (floor 60), each dimension may contribute the total split across the seven dimensions that fill it (floor 8), and each search query asks for chunks in proportion so a raised per-dimension cap is not starved by a top-12 request. At the default 120-file budget the list is 120 paths and 17 files per dimension; a budget at or under 60 behaves exactly as before.
- The budget is resolved once, from the manifest's own `[report].investigate_max_files` when it declares one and from the environment otherwise, so the caps size for what trusty-review will actually read rather than for a second value.
- The two ranking legs are now independent: a trusty-analyze daemon that will not start costs the complexity ranking and the `--analyze` enrichment, no longer the search-derived evidence as well.
- A search index that answers nothing, or query failures, are named gaps naming the repository and what the report therefore does not carry — never a silent fall back to path names.
- The complexity leg keeps the function it measured. `/complexity_hotspots`
  ranks chunks — one function each, with a name, a line range and a cyclomatic
  count — and the reader here parsed the file path and the count only, so
  collapsing chunks to files threw away the part that says WHERE in a 900-line
  file the complexity is. Each `inspect_priority` entry the complexity leg
  produces now carries a nested `hotspot = { function, start_line, end_line,
  cyclomatic }` table naming that file's worst function, and its `reason` line
  names it too. The file-level collapse is unchanged: one entry per file, the
  winning chunk's measurement recorded, the losing chunks still dropped.
  trusty-review reads the new key to point the investigation prompt at the
  function rather than the file (#6146).
- A manifest written before this parses exactly as it did — the key is absent,
  and an entry that has nothing else to say is still a bare path string. A
  daemon that ranks a chunk without locating it still ranks that chunk's file;
  the measurement is an addition to the entry, never a precondition for it.

## [0.7.0] — 2026-08-20

### Added

- Every run now writes an `index.md` into the directory it filled with reports — the sweep into `out/`, `trusty-audit render` into its `--out` directory — so the directory explains itself instead of being a pile of `.md` files and numbered subdirectories. It states the versions of `trusty-audit`, `tga`, `trusty-search`, `trusty-analyze` and `trusty-review` responsible for the run, the local time with UTC offset that it finished, the wall clock per repository (or per report) and overall, a table saying what every file and directory there is, and a relative link to each report file that exists. It is written unconditionally, a one-repository run included: "there is only one report" is a coverage fact worth stating, not a reason to skip the index ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- A repository or report that produced nothing is NAMED in that index with the reason and a link to its log, so a partial run's index says what is missing rather than being one section shorter and silent about it. A repository carried over from an earlier sweep is marked as such and keeps the duration of the run that actually audited it — the checkpoint now records that duration per repository ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- The timings are what the process measures and nothing more: one wall clock per repository around its `tga audit` child, one per report around its `trusty-review report` child, plus the run's own total. `tga` relays nine stages per repository but no stage event carries a duration, so the index says so rather than inventing a per-stage breakdown ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- The re-render's index reports the version its resolved `trusty-review` answers `--version` with, because a re-render has no install record to read and runs at whatever version is on the machine. `tga` is stated as "not recorded — a re-render runs no `tga`" rather than left out, and the index goes only into the `--out` directory: the source package is still read and never written ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- The return package now carries `reports/index.md`, so the explain-the-contents page reaches the recipient — who gets the zip, not the working directory the sweep wrote. It is generated for the package rather than copied from `out/index.md`, because the two directories differ: the package carries only the repositories that produced a report, carries no `logs/`, and puts `extract/` beside `reports/`. A copied index would have linked every failed repository's log into a file the archive does not contain. Its links are written relative to `reports/`, so a report resolves as `<stem>/…` and its extract database as `../extract/…`, both members of the same zip. A repository the package does not cover is still named in the index with the reason, and the package README's contents table now points at the index first ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- `trusty-audit render --from <package> --out <dir>` regenerates a finished audit's reports from the deliverable itself. It finds every `manifest.toml` under the unzipped package (`reports/<stem>/`) or a working directory (`out/<stem>/`) and runs one `trusty-review report --manifest … --analyze --out …` child per manifest — the same argument vector `tga audit` uses, so a re-render is the sweep's render step and not a second rendering path. It clones nothing, collects nothing, starts no daemon, and never writes into the package it read. Whoever received the deliverable can now regenerate it themselves; before this the only way to produce the report again was to re-run the whole sweep on the machine that collected the data, which needs git checkouts the package deliberately does not carry. There is no pin check: the verb runs at whatever version the resolved `trusty-review` is, resolved as `--review-bin`, else the copy under the working directory's `tools/`, else `PATH` ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- The re-render states what it cannot reproduce instead of letting a thinner report read as the same one: one gap line per repository whose checkout is not on this machine (the code scan and the analysis pass need it), and a closing note that the executive summary and top risks are model-authored, so a second render words them differently over the same figures — DOC-68 §12's recomputability boundary, said where it is actionable. A report that fails to render is recorded and the run continues, and a run that could not regenerate every report it found exits non-zero ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- The re-render takes its OpenRouter key from `OPENROUTER_API_KEY`, falling back to `engagement.toml` when there is one, and never prompts — the machine running it usually has no engagement config for a prompt to write a key into. The key reaches the renderer through its environment only, and the child's combined output is scrubbed of it before the log is written ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- An audit run now indexes each target repository with `trusty-search` and measures it with `trusty-analyze`, then writes the resulting complexity ranking into the manifest as `inspect_priority` — the interface trusty-review's investigation pass reads (#6078), which until now had no producer. The `render` verb does the same indexing and measuring before it invokes `trusty-review report --analyze`, so the tga-free render path no longer fetches from a daemon nobody started. Every leg is fail-open and every degradation is named twice: once in the run's own gap lines and once in the manifest's `[report].gaps`, so the rendered report states it too.

### Fixed

- The install package no longer carries the previous engagement's board credentials. `config::generate` is a substitution, so an auditor reusing one template shipped client A's `[boards.jira]` / `[boards.linear]` inside client B's zip — inside `engagement.toml`, which is deliberately readable by the recipient. The outbound scan in `package.rs` cannot catch it: that guards what leaves on the way back, not what was carried over on the way in. `distribute::assemble` now calls the new `config::generate_for_new_engagement`, which drops the `[boards]` table and reports the fields it dropped; the CLI names them under the package listing. Board credentials are the client's own — `[boards.jira]` is documented as "their credential, not the owner's" — so nothing an auditor's template holds there can belong to the engagement being generated, and a recipient registering a board with no credential already gets `AuditError::BoardCredentialMissing` naming the field to set. In-place re-rendering (saving a newly-entered key into the recipient's own config) still goes through `config::generate` and keeps their board credentials ([#5861](https://github.com/bobmatnyc/trusty-tools/issues/5861))
- `WorkDir::create` now proves something can be written where the root resolved before it creates anything. An operator with no resolvable home directory, standing somewhere they cannot write, used to meet `working directory /trusty-audit-work/tools: Read-only file system (os error 30)` — a message naming a directory that was never created. The new `AuditError::WorkDirNotWritable` names the root it needs, the directory it actually tried to write in, and both remedies (`cd` somewhere writable, or set `TRUSTY_AUDIT_WORKDIR`). The check is a probe directory created and removed in the nearest existing ancestor, not a mode inspection: `/` on macOS reads as writable from its bits and refuses the write. `workdir::default_root`'s claim that the cwd fallback is "at least writable" is corrected — nothing asserted it, and the check now covers every root rather than only that branch's ([#5941](https://github.com/bobmatnyc/trusty-tools/issues/5941))
- `cli::targets_file::detect` no longer stops at the first targets file it cannot open. An unreadable `repos.txt` used to return before `boards.txt` was read, so that file's bad lines went unreported and the operator fixed one problem, re-ran, and met the second. Read failures are now collected the way bad lines already were, and `AuditError::TargetsFileRefused` carries both lists — each under its own header, each rendering as nothing when empty. A single unreadable file with no bad lines still surfaces as `AuditError::Read` with the OS error as its `source` ([#5993](https://github.com/bobmatnyc/trusty-tools/issues/5993))
- `trusty-audit render` with no arguments now finds the engagement's own work dir. An engagement whose sweeps ran with `--work-dir <config-dir>/work` keeps its reports under `<config-dir>/work/out/`, which is neither the directory holding `engagement.toml` nor the tool's default work root — so a bare `render` from beside the config refused with `no manifest.toml under <config-dir>` while the reports sat one directory down. The chain is now the flag, then the directory you are in, then `work/` beside the engagement config when that config is really there, then the default work dir. Nothing in `engagement.toml` declares a work dir: `work/` beside it is the layout the flow produces, so the convention is read rather than a field added that every existing config in the field would be missing ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- A refusal now names every directory the run tried rather than only the one it settled on. `no manifest.toml under /Users/masa/dogfood-audit` named a directory the operator could already see and said nothing about the two others already ruled out ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- A `trusty-review` resolved off `PATH` is now disclosed in the run's own output, with the path it resolved to and the version it answered. A re-render performs no pin check, so when the work dir did not resolve the fall-through silently drove a `trusty-review` two minor versions behind the engagement's own pin and exited 0 — the only record was the versions table in an `index.md` the operator opens after reading the report. A renderer named with `--review-bin`, or the copy this engagement installed under `tools/`, is not announced: the line means something only because it is not on every run ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- A zero-argument `trusty-audit render` now drives the `trusty-review` from the work dir it discovered, not the one the front end defaulted to before discovery ran. The two were resolved separately: `resolve_source` found the engagement's reports under `<config-dir>/work`, and the renderer rung then looked under the tool's default work root, which held nothing — so `<config-dir>/work/tools/trusty-review` sat unused while a `PATH` copy two minor versions behind rendered the reports and the run printed the unchosen-renderer NOTE. The rung order is unchanged — `--review-bin`, then the discovered work dir's `tools/trusty-review`, then `PATH` with the disclosure — and the session's own root stays a second `tools/` candidate so a recipient standing in an unzipped package still gets the copy their engagement installed ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))
- A sweep now grounds every repository whose `tga audit` child left a manifest, rather than only those whose child exited 0. A child that failed at a later stage — a render whose synthesis failed, for instance — still wrote the manifest, and gating on its exit status skipped the `inspect_priority` ranking together with the gap line that would have said so. The failure the grounding legs promise to name always was named; the skip that stopped them running never was.

### Changed

- `trusty-audit` is now published to crates.io like every other workspace crate, reversing #5483's original call. Owner ruling: the client is ordinarily `cargo install`-able rather than distributed solely as the signed `.app`.
- `trusty-audit render` now works with no arguments at all from the directory the audit was unzipped into: the source defaults to that directory when it carries reports, the output to `rerendered/` inside it, the engagement config to the `engagement.toml` beside it, and the renderer to the installed `trusty-review` or the one on `PATH`. Before this the source defaulted to the working directory (`~/.trusty-tools/trusty-audit/work`), which on a recipient's machine holds nothing — so the one command they were given refused while they stood in the package it should have rendered. An operator who still has the engagement on disk is unaffected: a directory with nothing to render falls back to the working directory as it did. Every flag still overrides its default ([#6080](https://github.com/bobmatnyc/trusty-tools/issues/6080))

### Documentation

- `run::github_issues`' module docs no longer fail `cargo doc`. The header linked
  `crate::clone::record_selection`, which is a private `fn`, and rustdoc cannot
  resolve a link to a private item from a public doc — under
  `-D rustdoc::broken_intra_doc_links` that is an error, not a warning. It is now
  plain backticks, which says the same thing and resolves everywhere.

## [0.6.0] — 2026-08-19

### Added

- `trusty-audit clone <owner/name>...` acquires repositories the recipient has
  never checked out, into the working directory's `repos/` area — via
  `gh repo clone`, reusing the credential `gh auth login` already resolved. A
  clone is built under `state/clone-staging/` and renamed into place only after
  `gh` exits zero AND the tree verifies as a real checkout, so neither an
  interrupted run nor a commitless repository (which clones with exit 0 and no
  commits) is ever promoted as whole. One repository failing is a named gap and
  the run continues, exiting 2 so a shell chain does not proceed against an
  incomplete set; only every repository failing aborts. Clones are shallow by
  default and disk use is reported; `--budget-gb` stops new clones from
  STARTING once that much is on disk and does not cap a clone already running
  (#5215, DOC-68 §8 / §14 Q2).
- The auditor client has a desktop shell: `trusty-audit-ui`, a Tauri 2 window at `crates/trusty-audit/ui`. Phase 1 shows one view — the working-directory root, the engagement's manifest state, per-tool install status, and the next step — and adds no capability ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- The shell calls `Session::execute` in-process through a Tauri IPC command. There is no HTTP endpoint and no `taudit` subprocess, so a capability still has exactly one implementation and one CLI arm, per DOC-68 §11 ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- The window distinguishes a tool it installed and verified from one that is merely present, the same three states the CLI prints as `ok` / `UNVERIFIED` / `MISSING`. Wording is the front end's; the states come from `Session::execute` ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- A failed guided call shows its reason and a retry, rather than an empty panel that would read as an engagement with nothing to report ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- `trusty-audit discover` lists every repository the recipient's `gh` credential
  can reach — their own repositories plus each organization the account belongs
  to, not one named org. Every `gh` call routes through `trusty-common`'s single
  entry point, and every failure is a refusal naming the owner that could not be
  listed, never a silently shorter list (#5487, DOC-68 §6 / §14 Q4).
- `trusty-audit run` resumes an interrupted sweep instead of repeating it.
  `state/run-progress.toml` is now written after every repository rather than
  once at the end, so a crash, a timeout or a Ctrl-C costs the repositories
  still to come and none of the ones already audited
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- Each carried-over repository is named in the run's output as `resumed`, and
  the summary separates what an earlier run audited from what ran now. A
  repository being re-collected states why — its output was deleted, the
  earlier run recorded it as failed, or `--fresh` was asked for
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit run --fresh` audits every selected repository again, ignoring
  the record. Reach for it when the recorded outputs are stale rather than
  missing — re-cloned repositories, or a config change that should reach work
  already done ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- A repository the record calls audited is carried over only if its output is
  still on disk and still passes the same verification that accepted it the
  first time, and only if the current selection puts it at the same output
  path. Anything else is audited again
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit package` refuses a sweep that did not finish, naming the count
  recorded so far and pointing at the resume. Packaging one would have sent a
  partial engagement as a whole one
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit install` downloads and verifies the pinned `tga` / `trusty-analyze` / `trusty-review` triple, installing all three or none. It calls `trusty-installer`'s pinned, fail-closed entry point rather than implementing downloading a second time, so `cargo install` is unreachable and a checksum mismatch, an unpublished version, an unreachable host or a binary reporting the wrong version each install nothing ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- The engagement config carries the exact version triple under `[tools]`, as a bare version (`tga = "2.9.4"`) or with the artifact digest pinned too (`{ version = "…", sha256 = "…" }`). All three keys are required: a partly-pinned config does not parse, because an unpinned tool is one that resolves to whatever is current — the version-skew defect [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454) closed from the run side ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `state/tool-versions.toml` records the exact versions a successful install placed, so the deliverable can state which triple produced it ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- Global `--config <FILE>` names the engagement config that arrived with the handoff package; it defaults to `./engagement.toml` ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `trusty-audit package` assembles the deliverable to send back: one unencrypted zip carrying each audited repository's report directory, its tga extract database, and a generated `README.md` and `package.toml` stating which repositories were covered and at which tool versions. The path is printed as the last line, and `--out` writes it wherever the recipient will attach it from ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- The package refuses rather than ships when a member carries the engagement OpenRouter key, or when a symlink or hardlink under `out/` or `extract/` would pull in a file from outside the working directory. Hardlinks are caught by link count, since a hardlink is a second directory entry on the same file and no path- or type-based check can see one. Refusals leave no zip and no partial file behind ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- A sweep that audited nothing has no package, and a partial sweep's package names every repository it does not cover — in the returned value, in the printed output, in `package.toml`, and in a non-zero exit ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- New crate: the auditor client, a headless library with a thin CLI over it. Scaffold only — the capability set (`session::Command`), the working-directory layout (`workdir`), the engagement config and its non-serializable `SecretKey`, a reader for tga's `manifest.toml`, and the tool-install seam that fails closed pending `trusty-installer`'s pinned entry point. A bare invocation enters the guided flow. Every capability has a CLI invocation, enforced by an exhaustive match rather than by review ([#5502](https://github.com/bobmatnyc/trusty-tools/issues/5502))
- The binary installs under both names: `trusty-audit`, which the docs and the handoff README use, and `taudit` for repeat use. Two `[[bin]]` targets over one `src/main.rs` — the same arrangement as `trusty-installer` / `tctl` — so there is one implementation and no second code path to drift ([#5502](https://github.com/bobmatnyc/trusty-tools/issues/5502))
- `trusty-audit run` drives the audit sweep: one `tga audit` child per selected repository, using the pinned `tga` this client installed and verified rather than whatever is on `PATH`. A missing or unverified triple refuses the run and names what to install — there is no fallback, because an unpinned tool is the version skew [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454) closed ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- Per-repository results are captured, not just the sweep's overall exit code. A partial failure (one repository fails, others succeed) reads as `PARTIAL` and names which failed; a total failure says no repository was audited. Both exit non-zero, so a partial sweep can never be reported as a clean one ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/selected-repos.toml` is the run's input contract — `[[repositories]]` entries of `name` and `path`, with relative paths anchored to the work-dir root. Repository selection and cloning ([#5487](https://github.com/bobmatnyc/trusty-tools/issues/5487), [#5215](https://github.com/bobmatnyc/trusty-tools/issues/5215)) write this file; absent or empty is a refusal, not a zero-repository success ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/run-progress.toml` records what the last sweep did per repository. It is written after every child finishes, and a failure to write it fails the call rather than leaving a run that cannot be described ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `tga` is run from its absolute path and the report renderer is named to the child through `TRUSTY_REVIEW_BIN`, so neither comes from the operator's `PATH`. The analyze step cannot be pinned this way — `tga audit` invokes `trusty-review report --analyze`, which reads metrics over HTTP from a URL rather than spawning a binary — so `TRUSTY_ANALYZE_BIN` is deliberately left unset instead of set inertly ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- The engagement's OpenRouter key reaches the `tga` child by environment only — never a config file, a log line, or an error message ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- A zero exit from `tga audit` is not taken as proof anything was assessed. That command exits 0 whenever the sweep completed, failed stages included, so the client checks what the child produced: the manifest must exist, parse, name a repository, and state no failed `collect` stage. Other stated gaps are recorded and printed without failing the repository, per DOC-67 §9 ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- Per-repository files are stemmed `<index>-<name>`, so two repositories whose names sanitize alike — `acme/api` and `acme-api`, or `Acme` and `acme` on a case-insensitive filesystem — no longer share an output directory, a log file, or a database ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- The recorded tool versions are compared against the engagement's pins at run time, not only at install time. A config bumped between the two refuses the run instead of executing the older binary ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- A `tga audit` child that outlives four hours is killed and recorded as a timeout, so a hang costs one repository rather than an unattended run that leaves no record at all ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/selected-repos.toml` carries a required `count`, and a file holding fewer entries than it declares is refused — the partial write a producer crashing mid-write leaves behind, which otherwise reads as a smaller-but-complete selection. Writers must write a temporary file and rename it into place ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `run::save_selection` is the one writer of `state/selected-repos.toml`, honouring the two obligations that file's contract states: `count` ahead of the entries, and a uniquely-named temporary file renamed into place. The picker ([#5497](https://github.com/bobmatnyc/trusty-tools/issues/5497)) writes through it too, rather than re-deriving them ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- `tests/cli_end_to_end.rs` drives the whole chain through the real binary — several repositories selected, cloned and audited, with a stub `gh` and stub pinned tools and no network. Partial failure is exercised at both stages: a repository that cannot be cloned never reaches the sweep, and a repository that fails `tga audit` leaves the sweep `PARTIAL` and the process exiting non-zero ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- `trusty-search` is now a pinned tool alongside `tga`, `trusty-analyze` and
  `trusty-review`. `[tools]` in the engagement config gains a required
  `trusty-search` key, and `trusty-audit install` fetches the binary into
  `work/tools/`.
- `trusty-audit run` sets `TRUSTY_SEARCH_BIN` on every `tga audit` child, so the
  audit's search preflight starts the engagement's pinned copy instead of falling
  through to a PATH lookup on a clean machine.
- `--no-install` keeps the guided flow and `trusty-audit run` off the network,
  restoring the previous refuse-and-report behaviour exactly. `trusty-audit
  tools` never installs, with or without the flag, so a status read costs no
  download ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- `tools::unsatisfied` decides whether anything needs downloading from the same
  three conditions the sweep's preflight refuses on — the file is present, the
  version record names it, and that version equals the pin. An already-satisfied
  set constructs no HTTP client, so repeated invocations re-download nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- The guided flow and the `add` / `targets` help now coach registration breadth
  rather than waiting to be asked: applications, the repository holding the
  database schema or migrations, infrastructure and IaC, shared libraries and
  config repositories, and every ticketing board in use. The wording names what
  the assessment judges — how mature, how stable and how supportable the
  technology is — so the ask reads as audit quality rather than an inventory
  chore, and it claims no detection, because this client cannot see a target
  that was never registered. One `registry::COVERAGE_COACHING` constant, so the
  CLI and a later chat wizard present the same substance (#5822).
- `trusty-audit add repo <owner>/<name>` and `trusty-audit add board <jira|linear>:<KEY>` register what an engagement audits, additively — a second `add` never disturbs the targets already registered, and re-adding one is a no-op rather than an error ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- Each `add` proves the target can be READ before it is persisted, with the credential the sweep will later use: `gh auth token` then `gh repo view` for a repository, `GET /rest/api/3/project/<KEY>` for JIRA, a GraphQL team read for Linear. A target that cannot be reached is refused at registration, so it is not discovered as a gap an hour into an unattended sweep ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `trusty-audit targets` lists the registry and `trusty-audit remove <target>` drops one entry. Both accept `owner/name` or `provider:key`, and matching ignores ASCII case because GitHub, JIRA and Linear all do ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- The engagement config gained a `[boards]` table carrying the CLIENT's own JIRA (`url` / `email` / `token`) and Linear (`api_key`) credentials, as `SecretKey` values — no `Serialize`, redacting `Debug` and `Display`, the same rules the OpenRouter key has carried since [#5473](https://github.com/bobmatnyc/trusty-tools/issues/5473). Registering a board whose provider has no credential names the config field to set instead of returning an HTTP 401 ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `state/audit-targets.toml` is the registry file. It supersedes `state/selected-repos.toml` as the record of what the engagement TARGETS — it holds boards, which that file cannot express, and it exists before any clone. The selection file is unchanged and still what `trusty-audit run` reads as the record of what is on disk; `trusty-audit targets` names both so neither reads as lost ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `taudit install`, `taudit clone` and `taudit run` show live progress instead
  of waiting silently. The sweep reports the stages `tga audit` is actually
  running, relayed out of each child rather than swallowed into its log file —
  a sweep that used to show nothing for up to four hours per repository now
  names the stage in flight ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- `Session::with_progress` takes the sink a front end renders through, so
  `Session::execute` still runs with no terminal and the Tauri shell renders the
  same updates its own way. Absent, every update is discarded and the
  capabilities behave exactly as before
- Off a terminal — CI, a pipe, a captured run — the display degrades to one
  plain line per state change: no escape sequences, no repainting, and no line
  for a mid-flight counter
- `trusty-audit audit` runs a whole engagement in one invocation — install the
  pinned tools, clone the registered repository targets, sweep them with `tga
  audit`, and assemble the return package — instead of `install`, `clone`, `run`
  and `package` in order. The four separate verbs are unchanged.
- The chained run is resumable: interrupting it and running it again carries
  over installed tools, complete checkouts and audited repositories.
- A phase that fails names the phase it failed in — install, materialize,
  collect or package — rather than reporting "the audit failed".
- A sweep that audited nothing stops before packaging, so a failed collection
  cannot produce a zip that looks like a finished engagement. A sweep that
  partly failed still packages, names what it omits, and exits non-zero.
- `trusty-audit distribute` builds the install package that goes TO a client: one zip at `~/duetto/audit` (or `--out <dir>`) holding the `taudit` binary, an `audit.sh` launcher that runs it from wherever it was extracted, a generated `engagement.toml` carrying the OpenRouter key, and a README naming the three commands in order. The recipient extracts it and runs a script — no Rust toolchain, no `cargo install`, no PATH entry. The key comes from `OPENROUTER_API_KEY` when set and from the template config otherwise; there is deliberately no flag for it, because argv is visible through `ps` and lands in shell history ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- `config::generate` renders an engagement config carrying a credential — the one narrow path that writes a `SecretKey` into a file, kept beside the type whose missing `Serialize` makes every other path a compile error. The outbound return package's credential refusal is unchanged and unreachable from it: the two directions are two modules producing two types, never one function with a flag ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- A registered board is now collected instead of reported as a gap. `jira:ACME`
  or `linear:ENG` becomes a `jira:` / `linear:` section on each generated tga
  config, so `tga audit` syncs the board alongside the repositories. Registering
  a board previously stated it as a gap and held the whole engagement at a
  non-zero exit.
- `trusty-audit run` and `trusty-audit audit` ask for the OpenRouter key when nothing else supplies one, instead of refusing until someone hand-edits `engagement.toml`. The key is typed with the terminal's echo disabled, confirmed by a retype, and saved back to the engagement config at mode 0600. The prompt accepts either the key an auditor conveyed out of band or the client's own OpenRouter key, and reports which source a run used without ever printing the value ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `Session::with_credential` lets a front end hand in a resolved credential. The prompt lives in the CLI, so `Session::execute` stays callable by a front end with no terminal — the Tauri shell today, a TUI next ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `curl -fsSL <url>/crates/trusty-audit/install.sh | sh` installs and launches `trusty-audit` on macOS in one command. The script resolves a release (or an exact `TRUSTY_AUDIT_VERSION`), downloads the tarball and its published `.sha256` sidecar, refuses to continue on a digest mismatch, proves the downloaded binary reports a version before anything reaches `PATH`, installs into `${CARGO_HOME:-$HOME/.cargo}/bin` with an atomic rename rather than `cp`, and launches `trusty-audit` with stdin attached to `/dev/tty` so its credential prompt still works under `curl | sh` ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- The script is a bootstrap, not a second installer: its only job is getting the binary onto the machine. It checks only what gates whether the binary can run at all — a supported macOS architecture, the tools the script itself uses, the checksum, and that the binary executes. Provider reachability, the credential, and the collection dependencies (`gh`, JIRA, Linear) stay with `trusty-audit`, which already owns tool installation through `install_pinned_set` and can check each one at the point it needs it ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- An unsupported host is refused before any network call. Intel Macs are named explicitly: no `x86_64-apple-darwin` asset is published for any crate in this workspace, so the script says so rather than downloading an arm64 binary that cannot execute ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- Tagging `trusty-audit-v<version>` now publishes the `trusty-audit` and `taudit` binaries. The crate was absent from the release workflow's crate allowlist, so such a tag previously green-skipped and published nothing — the "a missing binary release is silent" caveat that allowlist documents ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- Launching on a terminal now walks the operator through registration instead of printing "Next: register the repositories and boards to audit (`trusty-audit add`)" and exiting. It asks for one target at a time, registers each through the same validation `trusty-audit add` runs, shows the running list as entries land, and on an empty line carries on into tool installation and — after asking — the sweep, all in the one invocation. A refused target is reported and the prompt returns; only the terminal itself failing ends the session.
- With no controlling terminal the launch is unchanged: it prints the status card and prompts for nothing, so scripts and CI keep the shape they had.
- Targets registered with `add` now advance the guided flow. It read only the companion `manifest.toml`, which `tga audit` writes after a sweep completes, so it kept telling an operator to register what they had just registered.
- The pins a cold start records come from the latest published stable release of
  each tool, resolved once and written into `engagement.toml`. Every run after
  the first reads the file, so `latest` never reaches a second run and the
  engagement states the exact triple it ran
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A synthesised `engagement.toml` names its `[models]` table in full: OpenRouter
  as the provider, `anthropic/claude-opus-4.8` for the judging call, and
  `anthropic/claude-haiku-4.5` for the verifier and summarizer. Leaving the table
  out fell through to `trusty-review`'s own defaults, whose provider is Bedrock —
  an account this engagement never named. All four fields are written because the
  table is all-or-none, and because it sits above the built-in constants in
  `trusty-review`'s precedence chain, these are the models the audit actually runs
  on ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A `repos.txt` or `boards.txt` beside `engagement.toml` is the engagement's target list, and the per-target prompt loop is skipped rather than seeded. Lines take short forms and browser URLs — `acme/api`, `https://github.com/acme/api/tree/main`, `linear:ENG`, `https://linear.app/<workspace>/team/ENG/active`, `https://acme.atlassian.net/browse/OPS-412` — and a `.git` suffix is stripped rather than becoming the repository's name, which `clone::split_name` would otherwise accept as the literal name `api.git`. A Linear URL yields the team key, never the workspace slug. Every target is registered through `Command::AddTarget`, so a file-detected one is validated and declared in `engagement.toml` exactly like a typed one ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- One unparseable line refuses the whole read: nothing from either file is registered, every bad line is named with its number and its own reason, and the operator is told their OpenRouter key is saved so the re-run does not ask for it. This holds with and without a terminal — an audit that silently covers fewer repositories than the file lists reports success over the absent ones ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- A review menu now ends registration, reached from both the targets file and the prompt loop: it states how many repositories will be cloned and how many boards collected from, then offers add, delete and proceed. Add and delete write through to `engagement.toml`. The menu choice discards typeahead before it is read, because a queued keystroke that can select `delete` is worse than one that can answer a yes/no. With no terminal there is no menu — the counts and the full list are printed and the run proceeds ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- Every registered repository now automatically contributes its own GitHub
  issues to the sweep's ticketing correlation — no separate board
  registration for a repo's own tracker. The generated `tga` config for each
  repository always carries a `github:` section naming that repository, using
  the recipient's own `gh auth token` credential (never a new raw-token config
  field) when one can be read, and running unauthenticated otherwise rather
  than silently omitting the section. A repository whose issues are disabled,
  whose credential is rejected, or whose repo is invisible to the resolved
  credential (a private repo with no or an invalid token) is reported the
  same way an unreachable JIRA project already is — a named gap on the
  affected repository, not an empty-but-successful ticketing section (#5980).
  A `gh`-derived token that reaches a child never lands in the run's log or
  in the packaged deliverable unredacted, the same guarantee `boards`'
  credentials already carry.
- `trusty-audit add repo /path/to/checkout` and a `repos.txt` line naming an
  absolute path now audit a repository that is already on disk, which is the
  only way in when the org it came from is no longer reachable from this
  machine's GitHub credential. An ABSOLUTE path is a checkout and anything else
  is a GitHub `owner/repo` — a syntactic rule, so the same `repos.txt` means the
  same thing on every machine. Acquisition is `git clone <path>`, which hardlinks
  the object store and reads the source only: **the source checkout is never
  modified** — not a fetch, not a checkout, not a stash (a stash list is shared
  across every worktree of a repository, so that is real damage, not a
  theoretical one). The checkout lands under `repos/local/<basename>` and flows
  through the existing sweep to a report and an `extract/<repo>.db`
  indistinguishable from a remotely-cloned one, carrying committed history only.
  A path that does not exist, is not a directory, is not readable, is not a git
  repository, is a subdirectory of one, is a SHALLOW clone, or has no commits is
  refused at registration naming which condition failed — a `--depth=1` source
  would report one commit by one author over a zero-length period (#5916), so it
  is refused rather than audited thinly. A bare mirror is accepted: it carries
  the whole history. Two paths whose basenames match would share one checkout
  directory and are refused at both gates, naming both paths (#6001).

### Fixed

- Installing refuses a `tools/` area that is a symlink or a file rather than a real directory. A pre-planted symlink would send the recipient's binaries outside the working directory and survive the `rm -rf` the README documents as a complete uninstall; the hazard was inert while nothing installed there ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `trusty-audit clone` now records what it acquired in `state/selected-repos.toml`, so `trusty-audit clone acme/api acme/web && trusty-audit run` audits both. Before this, no code anywhere wrote that file — [#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555) defined it and named repository selection and cloning as its producers, and neither implemented it — so the sweep refused with "nothing to audit" over a working directory full of checkouts ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- Only usable checkouts are selected. A repository that failed to clone is already a stated gap, and selecting it as well would fail it a second time for one cause, as a missing checkout in a sweep that had no way to know it was never acquired ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- An acquisition that produced nothing usable leaves an earlier selection alone, so a second `clone` that fails outright does not cost the operator the set that did land ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- The `tga audit` child now carries `TRUSTY_REVIEW_PROVIDER=openrouter` and the
  reviewer, verifier and summarizer model ids alongside the engagement's
  `OPENROUTER_API_KEY`, so review inference actually reaches OpenRouter. Naming
  the key was never enough: `trusty-review` resolves `Provider::Bedrock` as the
  last precedence level for all three roles, so the key sat unread while the
  reviewer either failed on missing AWS credentials or silently billed Bedrock
  ([#5671](https://github.com/bobmatnyc/trusty-tools/issues/5671)).
- The provider and the three model ids are resolved as one selection from a
  single layer — the operator's environment, else the engagement config, else
  the built-in slugs. A layer that names some of the four but not all four is
  refused before any child is spawned, naming what it set and what it left
  unset. Resolving them independently would pair an operator's
  `TRUSTY_REVIEW_PROVIDER=bedrock` with this crate's OpenRouter slugs, which is
  the same HTTP 400 reached from the other direction; nothing downstream catches
  that pairing ([#5679](https://github.com/bobmatnyc/trusty-tools/issues/5679)).
- An optional `[models]` table in `engagement.toml` overrides the built-in
  slugs, so a model rename is a config edit rather than a release. It rejects
  unknown keys, so `reviewr` is a parse error instead of a silent fallback to
  the default.
- A relative `--work-dir` (or `TRUSTY_AUDIT_WORKDIR`) no longer breaks
  `trusty-audit run`. `WorkDir::resolve` anchors a relative root to the caller's
  working directory, so the pinned `tga` binary, the generated tga config, the
  output directory and the extract database are all named absolutely to the
  child process — which runs with the work-dir root as its own cwd and
  previously failed to start with `os error 2`. (#5672)
- Two `trusty-audit add` runs against one working directory no longer discard each other's target. Registering and removing both load, mutate and save `state/audit-targets.toml`, and nothing made that sequence indivisible — the later save dropped the earlier one's entry while both reported success. Both now run under `trusty_common::file_lock::with_exclusive_lock`, the workspace's one implementation of that critical section ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- A `boards.jira.url` carrying `user:password@` userinfo no longer reaches a `Debug` render of the engagement config. The field is a plain string an operator may paste credentials into, and the derived `Debug` printed it verbatim; the userinfo is now stripped, leaving the site identifiable. The value itself is unchanged, so the request still carries it ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- A registered repository that failed to clone no longer vanishes from a
  one-shot `trusty-audit audit` run. Its failure was recorded only on the clone
  report, and because an unusable checkout is never selected, the sweep could
  not see it either — so the command exited 0 and handed back a package whose
  README said every repository was covered. The chain now folds the clone
  report's gaps into its own, which makes the exit status non-zero and puts the
  repository's name in the package's own `README.md` and `package.toml`.
- `trusty-audit distribute` names the member that actually failed. A write that failed on `README.md`, `audit.sh` or `engagement.toml` was reported as "taudit failed" — pointing the operator at the one member that had already succeeded ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- `Session::distribute` reads the packaging credential through an injected lookup rather than the live process environment, so `cargo test -p trusty-audit` can no longer write a developer's exported `OPENROUTER_API_KEY` into a zip on disk, and both credential sources are now asserted end to end ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- The return package could ship a repository's rendered report with its `extract/<stem>.db` collection database silently missing — `collect_extract` returned `Ok(())` whenever the database was absent, whether `extract/` itself did not exist or simply named nothing for that repository. Assembly now refuses with a named `MissingExtractDatabase` error instead, so the deliverable is always the database and the report together, never one without the other going unnoticed ([#5862](https://github.com/bobmatnyc/trusty-tools/issues/5862))
- The atomic state-file writer opens its temporary with `O_EXCL`, so it refuses whatever is already at that path instead of opening through it. The temporary's name is guessable — it is the pid plus a fixed-seed hash of a thread id — and a local attacker who pre-planted a symlink at it had the write follow the link: `trusty-audit`'s first-run prompt wrote the plaintext OpenRouter key into the attacker's file and left `engagement.toml` a symlink pointing there. Mode 0600 did not prevent it, because that mode constrains a file at creation and an open that follows a symlink creates nothing. A temporary left behind by a writer that crashed is still recovered, by unlinking and re-creating once ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `OPENROUTER_API_KEY` now wins over the key in `engagement.toml`. `trusty-audit run` used to hand the config's key to the `tga audit` child unconditionally, so an exported variable was silently ignored ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- A blank `openrouter_key` is refused before the sweep starts rather than an hour into it. A present-but-empty key was not a refusal anywhere downstream: `inference_env` read it as "select nothing" and returned no variables, so the `tga audit` child ran without its `TRUSTY_REVIEW_*` selection and `trusty-review` fell back to a provider nobody chose — surfacing at the report stage as a missing-AWS-credentials failure, or not at all ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `trusty-audit package` scans the outbound deliverable for the key the sweep actually used. With a key supplied through the environment, the scan checked the config's key instead and would not have caught the real one ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- A credential echoed by the `tga audit` child — or by the `trusty-review` it
  spawns — no longer reaches the per-repository log file. Both piped streams are
  filtered on the way to disk through `trusty_common::credentials::scrub_secrets`,
  the workspace's one redactor, and the relay path decodes the scrubbed bytes so
  a key quoted inside a stage detail never reaches the operator's terminal either
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The needle set is every credential the process can resolve
  (`resolved_secret_values`), not only the engagement's OpenRouter key: a `gh`
  token embedded in a git remote URL is a different credential from a different
  source, and the narrow set would miss it. It removes only values this process
  already holds, so the log is lower-risk rather than proven clean
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The output pump no longer buffers a line without bound. A child printing one
  endless line with no newline is flushed in bounded pieces
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The filter sees a credential in one contiguous search space, so neither of the
  two boundaries the pump used to invent can split one into unmatched halves. It
  scrubs its whole buffer before cutting a piece off to write, rather than
  scrubbing only the piece — a key lying across that cut used to reach the log in
  two verbatim halves. It also searches a mixed-encoding segment over its
  valid-UTF-8 runs concatenated, rather than one run at a time — a key is valid
  UTF-8 and so cannot contain an invalid byte, but a child can inject one into
  the middle of it, and the two flanking fragments used to reach the log in the
  clear. What the pump still holds back at a flush is only a credential that has
  not finished arriving, counted in text bytes so injected garbage rides along
  with it ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The size cap on that hold-back no longer writes out the credential it exists
  to keep. The hold-back is measured in text bytes and was capped in raw bytes,
  so around 4KB of non-UTF-8 output near a flush boundary — a binary diff or a
  corrupted pack object from a `git` child — pushed the cap past the text it was
  holding and wrote a partly-arrived key to the log in the clear; the rest of the
  key then arrived without its start and matched nothing, so both halves landed.
  The cap is now honoured by writing the invalid padding out of the hold instead,
  which leaks nothing, and the pump's buffer bound is unchanged at one segment
  plus the hold-back ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- `trusty-audit repos` no longer reads as a failed registration. It lists the companion `manifest.toml`, which `tga audit` writes once a sweep completes, so it is empty however many targets are registered — and its old empty-list message ("No repositories configured yet — run the guided flow to pick them") sent an operator who had just registered several repositories back to register them again. It now names `trusty-audit targets`, which answers the question that was actually asked.
- The README's verb table gains `add`, `targets` and `remove`, and states what separates `targets` from `repos`.
- The confirmed launch now runs the one-shot chain instead of the bare sweep. The
  sweep reads `state/selected-repos.toml`, which only `clone` writes and nothing
  in the guided launch wrote — so a fresh recipient died at
  `NoRepositoriesSelected` right after being told "Everything is in place", and a
  recipient carrying a selection from an earlier `taudit clone` had those OLD
  repositories audited, reported as audited, and exited 0.
- `trusty-audit guided` prints the status card and exits again. The interactive
  decision now reads the parsed CLI rather than the `Command` it maps to, which
  cannot tell the named verb from a bare launch — under a pty that turned the
  documented spelling into an unbounded hang.
- The sweep question discards terminal typeahead before it is asked, so an
  operator double-tapping Enter to finish adding targets no longer starts hours
  of unattended work with the second newline. Enter still starts the sweep.
- A board-only registry reports `SelectRepositories` again. Any registered target
  counted as a repository, so `taudit add board jira:ACME` skipped repository
  selection, triggered a real multi-tool download, and reported `ReadyForRun`.
- A bare launch prints its status card without asking for an inference
  credential. The key is resolved once the operator confirms the sweep, not
  before the card — a config with a blank key made the read-only launch exit
  non-zero after three mismatched retypes, never printing anything.
- A launch backgrounded with `&` prints the card instead of stopping silently.
  Opening `/dev/tty` for write succeeds from a background process group, so the
  first read raised SIGTTIN; the terminal probe now also checks the foreground
  group.
- The code-analysis leg reads the code again. `trusty-search` is default-deny,
  and nothing in the audit chain ever approved a clone, so `tga audit`'s
  `trusty-search index <checkout>` was refused on every repository of every
  run. It was silent: `tga audit` exits 0 whenever its sweep completed, so the
  run reported success and the report simply had nothing to say about the
  source. The sweep now runs `trusty-search index add <checkout>` before each
  child, and a refusal is a named per-repository failure rather than an empty
  section.
- Repositories are cloned with their whole history. `taudit clone` appended
  `--depth=1`, so tga read exactly one commit per repository and every
  deliverable said `commits=1`, `authors=1`, a period whose start equalled its
  end, and one author credited with the entire tree. Measured on
  `BurntSushi/xsv`: 1 commit by 1 author on 2025-04-24, against a real history
  of 407 commits by 30 authors from 2014-09-01. Every CSV was a header and one
  row.
- `trusty-audit-ui`'s guided-flow session builder (`guided.rs`) failed to compile against `WorkDir::resolve`'s new `home: Option<&Path>` parameter (#5929), which broke `origin/main` as a workspace. Both call sites — the production `session()` builder and its own test — now pass `dirs::home_dir()`, the same source `trusty-audit`'s CLI entry point uses, so the shell and the CLI keep resolving the same default work-dir root (#5935).
- A bare `trusty-audit` in a directory with no `engagement.toml` now sets the
  engagement up instead of registering targets against one that does not exist.
  It asks for the OpenRouter key first, writes `engagement.toml` at mode 0600
  with an exact version pinned per tool, preflights all four pinned tools,
  installs them, and asks what the audit covers LAST. Registration going first
  is what produced the reported launch — targets registered, `Tools: 0/4
  installed`, a command named to run next, and no key prompt at any point
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A cold start whose `engagement.toml` cannot be written now stops and says so,
  naming the file and stating that the key was not saved. It used to be
  unreachable: nothing wrote the file, so the three gates that hit its absence
  each degraded quietly and none of them named it
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- The scrubber that keeps a credential out of a spawned child's log, and the
  guard that refuses to package a file carrying one, both now cover the
  `gh`-derived GitHub token — previously only `EngagementConfig`'s own
  secrets were checked, so a rejected token echoed back by a child could
  reach the log or the deliverable unredacted (#5980).
- Packaging now refuses when the active `gh` credential differs from the one
  the sweep collected under, instead of silently scanning outbound files with
  the wrong token. Packaging can run as a separate process from the sweep —
  possibly under a different `gh` account after the operator re-authenticated
  — and the previous outbound scan re-resolved the credential fresh each
  time, so a token a child echoed at sweep time could ship in the deliverable
  unredacted because the newly-resolved token never matched it. A truncated,
  non-reversible fingerprint of the sweep's credential is now recorded in the
  checkpoint and compared at packaging time; a checkpoint written before this
  fingerprint existed still packages, with the uncertainty stated rather than
  silently assumed safe (#5980).
- `taudit add board linear:<team-id>` now registers the team's short key, so the
  board collects the issues it was registered for. Validation accepted a team id
  and stored it verbatim; collection matches `linear.team_keys` against the text
  before the hyphen in `ENG-1234`, which a team id never occupies, so the board
  validated green and contributed nothing on every subsequent run. The same
  round trip also stores Linear's own casing, which that exact-match filter is
  equally sensitive to.
- Registration now refuses a team whose key the sweep could never match, rather
  than persisting it. `validate` documented that a registered Linear key is one
  `is_linear_team_key` accepts and nothing enforced it: the reply's `key` field
  is optional, so a reply that omitted it registered an empty key — an entry that
  validated green, could never collect, and could not be removed either, because
  `taudit remove linear:` is not a spelling the parser accepts.
- A Linear team key already on disk is uppercased on its way to the sweep rather
  than skipped. tga reads a stored key twice and disagreed with itself: its
  collector compares team keys ignoring case, so `linear:eng` did collect
  `ENG-1234`, while its classifier compares exactly, so the same issue classified
  as nothing. Uppercasing keeps the collection and adds the classification.
- A Linear board that no normalisation can save is now stated as a gap on
  `taudit run` as well as `taudit audit`, and the run exits 2 rather than 0. The
  sweep resolved the boards, discarded the gaps, and returned "every repository
  audited" with no `linear:` section and nothing said — the silent skip the gap
  line exists to replace.
- That gap line now describes the key shape it needs instead of asserting the
  stored key is a team id, and names both halves of the remedy: registering the
  team key leaves the old entry behind, and a registry still holding it keeps
  reporting the board as not audited.
- A `repos.txt` or `boards.txt` written by Notepad, Excel's "Save as .txt" or PowerShell's `Out-File` starts with a byte-order mark, which is not whitespace and so survived `trim` into the charset check — refusing line 1 and, under the all-or-nothing rule, every repository in the engagement. The mark now comes off once at the file level; one anywhere else is still a malformed entry ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- `https://github.com/orgs/<org>/repositories` — the natural paste for "audit every repository in this org" — was read as the target `orgs/<org>`, so the operator got a refusal naming a repository they never listed. A URL whose first path segment is one github.com reserves for itself is now refused with a line saying to list each repository instead. `/issues/12`, `/blob/main/…` and `/tree/…` URLs still parse ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- A launch with no terminal whose targets files registered nothing exited 0, so a wrapper script reading `$?` saw success over a 0-of-14 outcome. Any shortfall now exits 2 (`EXIT_INCOMPLETE`) and prints how many of the listed targets did not register ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- Two local checkouts whose basenames differ only by case — `/srv/a/Apex` and
  `/srv/b/apex` — are now refused as a `CollidingCheckouts` at both
  registration and `clone_all`, instead of silently landing in one checkout.
  On a case-insensitive, case-preserving filesystem (APFS's default, and the
  one this feature runs on) `repos/local/Apex` and `repos/local/apex` are ONE
  directory, but the derived name kept its case, so the second repository was
  reported as audited having actually read the first one's history (the
  #5896 wrong-corpus family). The collision comparison in both gates is now
  case-folded, unconditionally.
- `trusty-audit add repo`'s usability check for a local checkout now asks
  `--is-bare-repository`, `--is-shallow-repository`, and `--show-toplevel` in
  one `git rev-parse` invocation instead of two, tolerating the toplevel flag
  failing on its own for a bare repository.

### Changed

- `trusty-audit tools` now reports the verified version beside each binary, and marks a binary this client did not place as `UNVERIFIED` rather than showing it as installed — a version it did not verify is one it cannot vouch for ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `Session::execute` is `async`. Installing downloads, and blocking on a runtime inside a synchronous `execute` would work for the CLI and then panic inside the Tauri shell, which calls from an async context ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- An engagement config that omits the `trusty-search` pin no longer loads. Add the
  key to any existing config before the next run.
- The guided flow and `trusty-audit run` now install the pinned tools they are
  missing instead of reporting them and stopping. The guided flow installs once
  repositories are chosen — the point it used to print "install the tools" — and
  `run` installs before the sweep's preflight. Both call the same all-or-none
  `trusty-installer` entry point `trusty-audit install` does, so a set that
  cannot be fully resolved installs nothing and the command fails; the #5454
  guarantee is reached earlier, not relaxed
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- A binary the client did not place — the `UNVERIFIED` row in `trusty-audit
  tools` — is reinstalled rather than kept. Nothing may claim a version for it,
  and an unknown version is the #5454 version-skew input; `tools/` is the
  client's own area under the work-dir root, so replacing it costs nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- The temp-file-then-rename discipline the state files rely on moved to `workdir::write_atomically`, so `selected-repos.toml` and `audit-targets.toml` share one writer rather than restating it ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- Each `tga audit` child's stdout and stderr are piped and teed rather than
  pointed straight at the log file. The per-repository log is unchanged in
  content; a repository whose child output could not be written to it is now
  recorded as a failure rather than left as a silent gap in the evidence
  ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- A registered repository target now reaches the sweep. `state/audit-targets.toml`
  had no reader until now — `run` took its input from the selection file only
  `clone` wrote — so registering a repository did nothing until someone cloned it
  by hand under the same name. The one-shot `audit` command clones what is
  registered.
- A registered board is reported as a stated gap rather than silently skipped.
  Passing one to `tga audit` would mean writing the board credential into the
  generated tga config on disk, which this client will not do.
- The board credential reaches `tga` the way the OpenRouter key already does:
  the generated `state/tga-<stem>.yaml` carries `${TRUSTY_AUDIT_JIRA_TOKEN}` /
  `${TRUSTY_AUDIT_LINEAR_API_KEY}`, and the sweep puts the real value in the
  child's environment. No secret is written to a file. An unset variable is a
  `tga` config error naming the field, not a silent unauthenticated read.
- A board is still stated as a gap when the engagement config carries no
  credential for its provider, and when a second JIRA project is registered —
  `tga`'s `jira.project_key` holds one.
- The return package refuses a file carrying the JIRA token or the Linear API
  key, as it already did for the OpenRouter key. Those two secrets now travel to
  a `tga audit` child, and the files that child writes are the files the package
  sends off the recipient's network.
- The child-log scrubber strips the board credentials too. It built its needles
  from the registered providers plus the OpenRouter key, and no provider there
  is a board, so a `tga` child quoting a JIRA token in an auth error wrote it to
  `work/logs/<repo>.log` in the clear. The packaging guard and the scrubber now
  draw their secrets from one list on `EngagementConfig`.
- `taudit audit` reads the target registry once. It used to read it again when
  the sweep started, hours later on a real engagement, so a board removed
  between the two reads left the report claiming coverage while nothing
  collected it — a zero exit over an engagement that skipped a registered board.
- `cli::registration::is_interactive` takes `&Cli` rather than `&Command`, and
  `guided_at_the_terminal` returns `Launch` rather than an `Outcome` so the
  front end owns when the credential is resolved.
- `README.md` no longer offers `TRUSTY_AUDIT_NO_LAUNCH=1` as a way to make the
  binary non-interactive. It is an `install.sh` variable deciding whether the
  installer starts the binary; the binary never reads it.
- The default working directory is `~/.trusty-tools/trusty-audit/work`, and the
  packaged launcher no longer pins `--work-dir` beside itself. The tree used to
  land wherever the recipient unzipped the package, and `trusty-search` refuses
  to index a checkout under `/tmp`, `/var/folders`, `~/Downloads`, `~/Desktop`
  or `~/Documents` — which is where an emailed zip gets opened — so the
  placement cost the code analysis its data on an ordinary machine. `--work-dir`
  and `TRUSTY_AUDIT_WORKDIR` still override, and the launcher forwards them.

  Two costs, both stated in the package README: approving a clone writes a row
  to `trusty-search`'s own allowlist, outside the work root, so deleting the
  root is no longer a complete uninstall (`trusty-search index remove <path>`
  undoes it); and an existing operator's tree does not move itself — pass
  `--work-dir` at the old location, or move it.
- The built-in reviewer default is `anthropic/claude-opus-4.8`, was
  `anthropic/claude-sonnet-4.6`. This changes behavior for EXISTING configs, not
  only for newly written ones: an auditor-supplied `engagement.toml` with no
  `[models]` table resolves through this constant, so the judging call on the
  common handoff path moves to the Opus analysis tier. The verifier and
  summarizer stay on `anthropic/claude-haiku-4.5`, and a config that names its
  own `[models]` table is unaffected — it outranks the built-in
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- `engagement.toml` now declares what an engagement audits, in a `[[targets]]` array, and `<work-dir>/state/audit-targets.toml` is a working copy rebuilt from it. An engagement is described by one portable file: hand someone the config and they have the key, the models and the scope ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- `taudit add repo` / `add board` / `remove` write the engagement config, under the same cross-process lock the registry took, and the config is published at mode 0600 before the working copy is mirrored — so a config write that fails leaves both files untouched ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- An engagement whose config declares no targets adopts whatever the working copy holds, so an upgrade keeps every target that was registered before this change; the first `add` or `remove` persists the adopted set into the config. `targets = []` is a declaration of zero and does not adopt ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- `taudit add` and `taudit remove` now refuse in a directory with no `engagement.toml`, naming the file and the command that creates one, rather than writing a registry nothing treats as authoritative ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))

### Removed

- `taudit clone --full` and `CloneOptions::shallow`. A full clone is now the
  only mode, so the flag had nothing left to select and the field was one
  assignment away from re-emptying the deliverable. Disk stays bounded by
  `--budget-gb`, which is unchanged: the same repository is 628 KiB shallow and
  1.2 MiB full, against a 20 GiB default.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

