# Runbook: Code-Only CAST Audit on Another Codebase

Run a CAST-style technical due-diligence report against a codebase outside
this workspace, using only what a repository checkout can tell you — no
interviews, no ops data, no organizational input.

## Required versions

This runbook targets `trusty-analyze report --template cast --code-only
--manifest <path>` — a command not yet released. It is tracked alongside the
CAST template's own gaps in [#6004](https://github.com/bobmatnyc/trusty-tools/issues/6004)
and the DD-dimension gap analysis in
[DOC-71](../specs/DOC-71-audit-dimensions-and-templates.md). Confirm it
exists before following this runbook: `trusty-analyze report --help` must
list a `--code-only` flag.

| Crate | Current version (this repo) | Needed |
|---|---|---|
| `trusty-analyze` | 0.12.4 | ≥ next release after 0.12.4 (adds the `report`/`--code-only` verb) |
| `trusty-review` | 0.31.1 | ≥ next release after 0.31.1 (adds the `code_only` marker plumbing) |
| `trusty-audit` | 0.12.1 | ≥ next release after 0.12.1 — only if you drive this through a full `trusty-audit` engagement instead of calling `trusty-analyze report` directly (see "Producing the manifest") |

`trusty-analyze report` embeds `trusty-review`'s report generator as a
library; the CAST template and its section-rendering logic live in
`trusty-review`, not `trusty-analyze` — both versions matter even though you
only invoke one binary.

## Prerequisites

- **Rust toolchain**, MSRV 1.94, via `rustup`:

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

- **Binaries:**

  ```bash
  cargo install trusty-analyze trusty-review --locked
  ```

  Add `trusty-audit` only if you plan to drive the run through a full
  engagement (`trusty-audit init` / `add repo` / `audit`) instead of calling
  `trusty-analyze report` directly against a hand-written manifest:

  ```bash
  cargo install trusty-audit --locked
  ```

- **Cloud credentials:** none, for a local checkout. Report generation always
  makes one LLM call (synthesis is unconditional since
  [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454)), so you need
  one of:

  ```bash
  export OPENROUTER_API_KEY=sk-or-v1-...
  # or leave AWS Bedrock credentials in ~/.aws/credentials / the AWS env vars / an IAM role
  ```

  Neither key is written into the rendered report or the manifest — a
  manifest's `[inference]` section carries provider/model identity, never a
  credential (`crates/trusty-review/src/report/manifest.rs`).

- **`GITHUB_TOKEN`** — optional. Only needed if a manifest repository entry
  uses `remote = "owner/repo"` instead of a local `path`, or if you separately
  enable `trusty-review`'s GitHub Issues context source
  (`TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_ENABLED=true`). A code-only audit
  against a local checkout needs neither.

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

Optional keys worth setting for a code-only run:

```toml
[report]
title = "FLYR Technical DD"
analyst = "Matt"

[[repositories]]
name = "FLYR"
path = "<repo-path>"
ref  = "main"
```

`--instructions <file>` (or the manifest's `[report].instructions` key) can
steer emphasis in the executive summary and findings; it never authorizes
inventing a fact.

If you instead drive this through `trusty-audit` (multiple repositories, or
you want `tga`'s git-history sweep alongside the code scan), `trusty-audit
add repo <repo-path>` and `trusty-audit audit` write and consume this same
manifest shape for you — see `crates/trusty-audit/README.md`. That path is
out of scope for the rest of this runbook, which assumes the direct
`trusty-analyze report` invocation the task names.

## Running the audit

```bash
trusty-analyze report \
  --template cast \
  --code-only \
  --manifest report-manifest.toml
```

`trusty-review`'s own `report` subcommand takes the same manifest and accepts
`--out <dir>` (default `./reports`), `--instructions <file>`, `--no-mermaid`,
and `--analyze`; `trusty-analyze report` is expected to carry the same flags
since it calls the identical generator — confirm the exact set with
`trusty-analyze report --help` once the command ships, rather than assuming
this list is final.

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

**Expected runtime.** Scales with repository size and the number of LLM
calls, not with binary download time. No fixed SLA exists in the codebase —
a first run against a large, previously unindexed repository takes
noticeably longer than a small one. Give it time rather than assuming a
hang.

## Reading the report

**Provenance markers.** Every substantive fact carries a trailing
superscript, defined once in a legend near the top of the report:

| Marker | Meaning |
|---|---|
| ⁽ᵐ⁾ | **Measured** — computed directly from the repository (LoC, complexity, dependency versions) |
| ⁽ᵈ⁾ | **Declared** — taken from the manifest or an analyst-supplied value (a day rate, a model name) |
| ⁽ⁱ⁾ | **Inferred** — an LLM judgment grounded in cited evidence (file:line), never a bare guess |

A genuinely unknowable field is dropped rather than shown with a marker.

**Code-only markers.** `--code-only` adds two more things to watch for,
beyond the three-way legend:

- A line ending **"Inferred from code; not validated by interview or ops
  data."** — the section rendered real, code-derived content, but nothing in
  it was cross-checked against a conversation or an ops dashboard the way a
  full CAST engagement would. Treat it as directionally useful, not as a
  validated finding the way a ⁽ᵐ⁾ fact is.
- A cell reading **"Out of scope for a code-only audit — requires
  [interview / CAST's proprietary benchmark corpus / ops metrics], not
  available from repository inspection alone."** — trusty deliberately did
  not attempt that measurement. This is the run's stated boundary, not a bug
  or a missing feature; the section heading still renders so the gap is
  never mistaken for a silent omission.

**The CAST 1.00–4.00 scale.** The CAST template documents CAST's own scoring
model: five health factors (Robustness, Efficiency, Security, Changeability,
Transferability), each scored 1.00 (very-high-risk) to 4.00 (low-risk) from
rule-violation percentages, rolled up into a TQI. Age-adjusted acceptability
bands apply (`<2yr` expects `>3.40`; `2–5yr` `>3.20`; `5–10yr` `>3.00`;
`>10yr` `>2.70`). As of this writing, not every health-factor cell has a real
computation behind it — some render the code-only PARTIAL marker rather than
a genuine 1.00–4.00 score; check the report's own Gaps & Caveats section for
which cells are live versus placeholder for your run.

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

- **Large-repo runtime.** A large, previously unindexed repository costs
  noticeably more wall-clock time than a small one — both in the file scan
  and in the LLM calls. There is no fixed SLA; a run that is still making
  progress (growing log output, no error) is not stuck.
- **Missing `OPENROUTER_API_KEY` / AWS credentials.** The run stops before
  reading any repository, naming the missing credential — this is the
  intended fail-fast behavior (#5454), not a bug.

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
