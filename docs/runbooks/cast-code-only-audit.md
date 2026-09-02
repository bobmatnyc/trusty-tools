# Runbook: Code-Only CAST Audit on Another Codebase

Run a CAST-style technical due-diligence report against a codebase outside
this workspace, using only what a repository checkout can tell you — no
interviews, no ops data, no organizational input.

This runbook targets `trusty-analyze report --template cast --code-only
--manifest <path>`. It is tracked alongside the CAST template's own gaps in
[#6004](https://github.com/bobmatnyc/trusty-tools/issues/6004) and the
DD-dimension gap analysis in
[DOC-71](../specs/DOC-71-audit-dimensions-and-templates.md).

## Prerequisites

- **Rust toolchain**, MSRV 1.94, via `rustup`:

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

- **Binaries:**

  Path A — use `trusty-analyze report` directly against a hand-written manifest:

  ```bash
  cargo install trusty-analyze --features review --locked
  cargo install trusty-review --locked
  ```

  Path B — drive the run through a full `trusty-audit` engagement (`trusty-audit
  init` / `add repo` / `audit`), which coordinates the same pipeline:

  ```bash
  cargo install trusty-audit trusty-review --locked
  ```

  `trusty-analyze report` embeds `trusty-review`'s report generator as a
  library — both binaries reach the same implementation entry point. The
  `--features review` flag is required for `trusty-analyze report`; without it,
  the `report` verb and `--code-only` flag are unavailable. `trusty-review
  report` is that same generator's own front door. It needs no extra feature
  flag: `cargo install trusty-review --locked` is enough on its own.

- **LLM credentials:** Required. Report generation always makes inference calls
  [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454). Provide one of:

  ```bash
  # OpenRouter (default provider):
  export OPENROUTER_API_KEY=sk-or-v1-...

  # or AWS Bedrock (standard credential chain):
  export AWS_REGION=us-east-1            # if not already set
  export TRUSTY_LLM_MODEL=bedrock/us.anthropic.claude-sonnet-4-6
  # AWS credentials via env vars, ~/.aws/credentials, IAM role, or SSO
  ```

  Neither credential is written into the rendered report or the manifest — a
  manifest's `[report]` section carries provider/model identity, never a
  credential. The run stops before reading any repository if credentials are
  missing, naming the missing key.

- **`GITHUB_TOKEN`** — optional. Only needed if a manifest repository entry
  uses `remote = "owner/repo"` instead of a local `path`, or if you separately
  enable `trusty-review`'s GitHub Issues context source
  (`TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_ENABLED=true`). A code-only audit
  against a local checkout needs neither.

- **Index resolution for `--analyze`** (#6677): if you use `--analyze`, the
  report resolves the trusty-search index by the checkout's registered
  `root_path` when the derived index id is absent from the daemon's registry,
  and logs a WARN line naming both the derived and the resolved id. A
  checkout indexed under any id still works.

## Producing the manifest

`trusty-analyze report --manifest` takes a `trusty-review` report manifest —
a single TOML file. There is no generator command for an arbitrary,
unregistered repository; write it by hand, following the schema in
`crates/trusty-review/src/report/manifest.rs` and documented in
`crates/trusty-review/README.md` ("Manifest").

Minimum manifest for one local checkout at `<repo-path>`:

```toml
# report-manifest.toml
[report]
title = "FLYR Technical DD"

[[repositories]]
name = "FLYR"
path = "<repo-path>"
```

`path` must be a local checkout — `trusty-analyze report --manifest` reads
tracked files directly (`git ls-files`, or a filtered walk for a non-git
directory) and needs no prior indexing. Local entries get deterministic git
enrichment (branch, short SHA, origin, dirty flag) for free.

Optional keys for a code-only run:

```toml
[report]
title = "FLYR Technical DD"
analyst = "Matt"
code_only = true

[[repositories]]
name = "FLYR"
path = "<repo-path>"
ref  = "main"
investigate_max_files = 40
```

Key notes:
- `[report] code_only = true` is optional in the manifest; the `--code-only` CLI
  flag also sets it. The flag turns the mode ON only — omitting it never widens
  a scope the manifest declared.
- Each `[[repositories]]` entry declares **exactly one** of `path` (local
  checkout) or `remote` (`owner/repo` for GitHub repositories).
- `investigate_max_files` caps how many files are sent to the LLM per repository;
  default is built-in (roughly 40 files). `--investigate-max-files` CLI flag
  takes precedence.
- `ref` is optional; when omitted, the current checked-out branch is used.
- `metrics` can reference a pre-produced `trusty-analyze` metrics JSON file;
  when omitted, a scan is performed.

**Evidence discovery.** A manifest declaring only `[report]`, `[inference]`,
and `[[repositories]] name/path` gets no search-derived evidence. The
2026-09-02 live run against this checkout reported: "evidence discovery:
path-name heuristics only — the manifest declared no search-derived evidence
for this repository." This is a prerequisite, not a permanent gap:
`crates/trusty-review/src/report/manifest.rs` documents the
`inspect_priority` key for exactly this purpose.

```rust
/// Ranked repo-relative paths the investigation pass must inspect first
/// (#6078). Empty — the default — leaves selection byte-identical to a
/// manifest without the key. See [`InspectionPriority`].
pub inspect_priority: Vec<InspectionPriority>,
```

The companion `[report].attributed_only` key governs what happens when that
declared list runs short. Its doc comment: "Select ONLY files this manifest
declared, never padding with path-name heuristics (#6082)." Set `true`, a
short `inspect_priority` list renders as a stated shortfall in Investigation
Coverage, never padded with heuristic-scored files.

Each entry also accepts `dimension` and `reason` (#6082), attributing the
file to a DD dimension and naming the query that found it. `trusty-audit`
writes this automatically from its search and knowledge-graph ranking; a
hand-written manifest can declare it too. Track follow-up work in
[#6669](https://github.com/bobmatnyc/trusty-tools/issues/6669).

`--instructions <file>` (or the manifest's `[report].instructions` key) hands
the generator a free-form markdown brief. It steers emphasis in synthesis (if
enabled) but never authorizes inventing a fact.

If you instead drive this through `trusty-audit` (multiple repositories, or
you want `tga`'s git-history sweep alongside the code scan), `trusty-audit
add repo <repo-path>` and `trusty-audit audit` write and consume this same
manifest shape for you — see `crates/trusty-audit/README.md`. That path is
out of scope for the rest of this runbook, which assumes the direct
`trusty-analyze report` invocation the task names.

## Running the audit

```bash
trusty-analyze report \
  --manifest report-manifest.toml \
  --template cast \
  --code-only \
  --out ./dd-reports
```

`trusty-analyze report` fetches trusty-analyze metrics on demand by default;
pass `--no-analyze` to skip that fetch. `trusty-review report` defaults the
same fetch OFF, so run it with `--analyze` explicitly, or the complexity
profile renders "not stated in source data":

```bash
trusty-review report \
  --manifest report-manifest.toml \
  --template cast \
  --code-only \
  --analyze \
  --out ./dd-reports
```

Both binaries call the same generator and accept nearly the same flags. The
analyzer-metrics fetch is the one exception: `trusty-analyze report` takes
`--no-analyze` (on by default); `trusty-review report` takes `--analyze` (off
by default), listed below as the shared table's one binary-specific row.

| Flag | Default | Meaning |
|---|---|---|
| `--manifest <FILE>` | required | The report manifest TOML file |
| `--template <NAME>` | `report-technical-dd` | Template name: `cast`, `default`, `generic`, or a full/override name |
| `--code-only` | off | Mark non-code sections as out-of-scope; mark code-derived sections as inferred |
| `--out <DIR>` | `./reports` | Output directory for the markdown + JSON pair |
| `--instructions <FILE>` | unset | Free-form analyst brief (markdown) for synthesis focus |
| `--corpus <DIR>` | XDG data dir | Deterministic cross-repo benchmark corpus directory |
| `--benchmark` | off | Compute percentile/quartile placement in the corpus |
| `--analyze` (trusty-review report only) | off | Fetch trusty-analyze metrics on demand for local-path repos declaring no `metrics` file, only when already indexed. Fully fail-open |
| `--no-mermaid` | off | Skip Mermaid chart generation beneath graph-ready data tables |

`--synthesize` is deprecated and ignored (#5454): synthesis now runs
unconditionally on every report. Passing the flag only prints a deprecation
line to stderr and changes nothing.

Template aliases: `cast` → `report-technical-dd-cast`, `default` / `generic` →
`report-technical-dd`. Override either by dropping a same-named file in
`~/.trusty-review/templates/`; the override directory is checked before the
bundled default.

**Where it lands.** The generator writes a markdown + JSON pair per run,
`{slug}.md` / `{slug}.json`, under the output directory (`./reports` unless
overridden). The JSON twin carries the same data as the markdown, including
which models ran per role — read it if you need the report's data
programmatically rather than by scraping the markdown.

**Rendering to PDF/HTML.** Nothing in this workspace converts the report to
PDF or HTML — hand the markdown to a general-purpose converter (e.g.
`pandoc report.md -o report.pdf`) if a client wants a non-markdown
deliverable. Mermaid charts render fine on GitHub and most markdown viewers;
a converter that does not support Mermaid fences will drop them.

**Expected runtime.** Measured once, 2026-09-02, one repository: a
103082-chunk index of this checkout took about 3m45s wall-clock, using
`trusty-review report --code-only` with the default OpenRouter roles
(reviewer, verifier, and summarizer). Most of that time is LLM synthesis.
The scan-only phases finish in well under a minute. Treat this figure as one
data point, not an SLA. Runtime scales with the file investigation budget and
the number of LLM calls, so a larger `investigate_max_files` costs more. A run
that is still logging output is not stuck.

## Reading the report

**Provenance markers.** Every substantive fact carries a trailing
superscript, defined once in a legend near the top of the report:

| Marker | Meaning |
|---|---|
| ⁽ᵐ⁾ | **Measured** — computed directly from the repository (LoC, complexity, dependency versions) |
| ⁽ᵈ⁾ | **Declared** — taken from the manifest or an analyst-supplied value (a day rate, a model name) |
| ⁽ⁱ⁾ | **Inferred** — an LLM judgment grounded in cited evidence (file:line), never a bare guess |

A genuinely unknowable field is dropped rather than shown with a marker.

**Code-only markers.** `--code-only` adds two categories of output:

- **Non-code sections** (Peer Benchmark, Next Steps) render with an explicit
  marker: "Out of scope for a code-only audit — requires <what it needs>, not
  available from repository inspection alone." The section heading still renders
  so the gap is never mistaken for a silent omission.
- **Partial sections** (OSS/CVE exposure, License/IP risk, Remediation Economics)
  render their code-derived content with an added marker: "Inferred from code;
  not validated by interview or operational data." Treat these findings as
  directionally useful, not as validated facts the way a ⁽ᵐ⁾ measurement is.

The Report Metadata table states the scope, so a reader never has to infer it.

**The CAST 1.00–4.00 scale.** The CAST template renders CAST's scoring model:
five health factors (Robustness, Efficiency, Security, Changeability,
Transferability), each scored 1.00 (very-high-risk) to 4.00 (low-risk). Known gap:
not every health-factor cell has a computation behind it yet. The report's own
"Gaps & Caveats" section names which cells are live versus placeholder for your
run. This gap is tracked in #6004.

**trusty's own Code Quality / Security / Performance sections are NOT
CAST-scored.** The default (non-CAST) template carries these sections on
trusty's native 0–100 scale (RED `<33` / AMBER `33–66` / GREEN `>66`) — a
different scale from CAST's 1.00–4.00. The CAST template variant currently
defers these sections outright (tracked in #6004); if a future version of
the CAST template includes them, they still report on trusty's native scale,
not CAST's. Do not read a number from one of these sections as a CAST health
factor, or vice versa.

## Troubleshooting

- **Missing language adapter.** `trusty-analyze` ships adapters for Rust,
  TypeScript/JavaScript, Python, Go, Java, C#, C, C++, PHP, Ruby, Scala,
  Swift, and Kotlin (`crates/trusty-analyze/src/lang/adapters/`). There is no
  SQL / stored-procedure adapter and no Roslyn-based `.NET` linter in
  `run_diagnostics`'s tool roster — a codebase with a large SQL or .NET
  surface gets weaker per-file findings there than CAST's own
  Stored-Procedures deep dive. This is a known gap, not a misconfiguration.
- **`SKIP_UI_BUILD=1` needed.** `trusty-analyze` embeds a Svelte UI built by
  `build.rs` via `pnpm`. `cargo install --locked` from crates.io ships a
  prebuilt UI and does not trigger this; building from a git checkout
  without `pnpm` on `PATH` does. Set `SKIP_UI_BUILD=1` before the build to
  skip it:

  ```bash
  SKIP_UI_BUILD=1 cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
  ```

- **Engagement-level configuration.** If driving through `trusty-audit`, set
  `[report] template = "cast"` and `code_only = true` in the engagement
  manifest; `trusty-audit` passes these to the child renderer as environment
  variables `TRUSTY_AUDIT_REPORT_TEMPLATE` and `TRUSTY_AUDIT_REPORT_CODE_ONLY`.
- **Missing LLM credentials.** The run stops before reading any repository,
  naming the missing credential — this is the intended fail-fast behavior
  (#5454), not a bug.

## What this run does NOT cover

A code-only audit is bounded by what a repository checkout can tell you.
These CAST sections have no code-derived equivalent and render as explicitly
out of scope, never silently blank:

- **Peer Benchmark** — CAST's industry-quartile/rank placement needs their
  proprietary corpus of thousands of scanned applications; no comparable
  population exists here.
- **Next Steps (organizational/process recommendations)** — "empower Scrum
  Master," team-behavior change, modernization-readiness timing — inherently
  organizational, not derivable from source.
- **Interviews** — no interview transcript section exists in this run's
  output, and none is collected.
- **Ops metrics / cloud bills** — no cloud-spend, infrastructure, or
  operational-metrics data is read or reported.
- **Team org-chart / team structure** — no org-chart or team-structure
  section exists in this run's output.
