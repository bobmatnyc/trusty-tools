# C# lint support for `trusty-analyze` — development plan

**Issue:** [#916](https://github.com/bobmatnyc/trusty-tools/issues/916)
**Date:** 2026-06-08
**Status:** Plan (approved approach: project-scoped, real-disk build-then-filter)
**Scope:** Add a C#/.NET lint-class diagnostic adapter to `trusty-analyze`'s
`run_diagnostics`, closing the only language gap among the 10 existing linters.

---

## 1. Goal

`run_diagnostics` ships 10 adapters (clippy/ruff/biome/staticcheck/pmd/rubocop/
phpstan/swiftlint/detekt/clang-tidy) but **no C#/.NET** one. C# already has
first-class *syntactic* coverage via the tree-sitter `CSharpAnalyzer`
(`lang/adapters/csharp.rs`, tag `"csharp"`, ext `.cs`) feeding
`analyze_quality` / `complexity_hotspots` / `find_smells`. What's missing is the
**semantic, lint-class** signal — exactly the part that requires a compilation.
This plan adds a Roslyn-backed adapter that emits that signal.

## 2. Decisions (carried from the issue + verified against the code)

1. **Runner = the .NET SDK, not a standalone linter.**
   `dotnet build -p:ErrorLog=out.sarif,version=2.1` is the only path that emits
   SARIF natively on Linux and captures **both** compiler and analyzer
   diagnostics in one file. Any analyzer NuGet you add (StyleCop.Analyzers,
   SonarAnalyzer.CSharp) flows through the same output at no extra build cost.
   You pick the SDK as the runner and choose which analyzer NuGets to load —
   you do not pick "a linter." Gate on `which("dotnet")`, **not**
   `which("roslynator")` (Roslynator's CLI emits only XML/GitLab, never SARIF).

2. **Write a dedicated, C#-private SARIF parser** — do **not** generalize
   `parse_detekt_sarif`. Matches house convention (8/10 adapters carry their own
   parser + own `severity_from_str`) and decouples the Kotlin/detekt path from
   Roslyn's SARIF quirks (Roslyn output is native-but-not-strictly-conformant
   2.1.0). Same SARIF version, separate code path.

3. **Follow the clippy/rust adapter pattern: build the whole project, then
   filter to the target file.** `tool_impls/rust.rs::run()` runs whole-crate
   `cargo clippy` in `file.parent()` and `parse_clippy_json(stdout, file)`
   filters results to the one file. C# is the same shape — build the
   compilation unit (`.csproj`), filter SARIF `results[]` by
   `physicalLocation.artifactLocation.uri` — just with NuGet restore added and
   MSBuild instead of cargo. **clippy is the sibling to copy, not detekt.**

## 3. Code reconnaissance — what the plug-in surface actually is

Verified file-by-file (paths relative to `crates/trusty-analyze/src/`):

| Concern | Location | Note |
|---|---|---|
| `StaticTool` trait | `core/tools.rs:58` | `name`/`language`/`is_available`/`run(file, content)`; **must never panic** — a missing binary / non-zero exit / bad output returns `Ok(vec![])`, never `unwrap`. |
| `ToolDiagnostic` | `core/tools.rs:33` | `tool, file, line, col, severity, code: Option<String>, message`. |
| `Severity` | `core/tools.rs:21` | `Error \| Warning \| Info \| Hint`, serde `rename_all="lowercase"`. |
| SARIF parser to mirror | `tool_impls/kotlin.rs` | `parse_detekt_sarif` → `sarif_result_to_diag` → `severity_from_str`; lenient walk of `runs[0].results[]`, no schema validation. |
| Build-then-filter precedent | `tool_impls/rust.rs` | `parse_clippy_json` + `file_matches(span_file, want)` (suffix-tolerant path match). |
| Shell-out helper | `tool_impls/mod.rs::run_command` | **Hard 30s wall-clock timeout** (`TOOL_TIMEOUT`). Kills the child on overrun. |
| Registry | `core/tool_registry.rs:48` | `discover()` builds an `all_tools: Vec<Arc<dyn StaticTool>>`, keeps the available ones bucketed by `language()`. Add `Arc::new(RoslynTool)` here. |
| mod exports | `tool_impls/mod.rs` | `pub mod kotlin;` + `pub use kotlin::DetektTool;` — mirror for `csharp`. |
| Tool descriptor doc | `mcp/descriptors.rs:70` | `language` is a **free-form string**, not a real enum — its doc string lists the supported tags. "Add csharp to the enum" = append `csharp` to this string. |
| Language detection | `core/complexity.rs:66` (`"cs" => "csharp"`), `lang/adapters/csharp.rs:47` (`.cs`) | `.cs` → `"csharp"` already works; routing is free. |

## 4. The two blockers the original research missed

Both are **harness-level**, not parser-level, and both are why this feature is a
dispatch-architecture change rather than a 100-LOC drop-in.

### 4.1 The dispatch harness isolates every file into a scratch tempdir

`service/mod.rs::run_diagnostics_blocking` (≈`:660`) is the only path behind
`run_diagnostics`. For each file it:

1. fetches chunks from the trusty-search index (`fetch_chunks`),
2. reconstructs whole-file content by keeping the **longest single chunk** per
   file (`service/mod.rs:635`) — *incomplete if chunks don't cover the file*,
3. writes that content to a fresh `tempfile::tempdir()` under its **basename
   only** (`:692`),
4. runs the tool against that scratch path (`:704`),
5. rewrites `d.file` back to the index-relative path afterward (`:710`).

Consequences:

- A lone `.cs` file in an empty scratch dir has **no `.csproj`, no sibling
  files, no restored packages, no project** — Roslyn cannot semantically
  compile it. A naive clippy-clone `run()` returns nothing, every time.
- **This already silently breaks clippy.** `cargo clippy` in a scratch dir with
  one `.rs` file and no `Cargo.toml` errors to stderr and yields zero
  diagnostics. The build-then-filter pattern the issue cites as precedent is not
  actually exercised by the live `run_diagnostics` path. *(File a separate issue
  to confirm + fix clippy's harness path; the fix below repairs both at once.)*
- The longest-chunk reconstruction is also lossy for a compile — another reason
  to read the real file from disk, not rebuild it from chunks.

### 4.2 The 30-second timeout

`run_command` hard-kills at 30s. A cold C# path = NuGet restore + full MSBuild
compile, routinely well over 30s; per-file invocation pays it repeatedly. The
build-class tools need an elevated/elastic timeout and warm caches.

## 5. Path-model facts (verified, drives the fix)

- trusty-search stores chunk `file` paths **root-relative** — the reindex
  pipeline strips `root_path` from every walked file
  (`trusty-search .../watch_loop.rs:181`, `core/store.rs:145`).
- The real project root lives on the search-side `IndexHandle.root_path`
  (`trusty-search core/registry.rs:206`).
- The analyze client does **not** currently receive it: `IndexSummary` carries
  only `{ id }` (`trusty-analyze core/client.rs`), and `get_chunks` returns
  chunks with root-relative `file`.
- Therefore the absolute on-disk path of any indexed file is
  `index.root_path.join(chunk.file)` — but the analyze daemon must first be
  taught `root_path`. **This is the one cross-crate plumbing step the feature
  needs.**

## 6. Plan — phased

### Phase 0 — Foundation adapter + parser (mergeable alone, zero behavior risk)

Lands real, fully unit-tested code that compiles into the registry even before
the dispatch fix exists.

- **New** `tool_impls/csharp.rs` → `pub struct RoslynTool`:
  - `name()` → `"roslyn"`
  - `language()` → `"csharp"`
  - `is_available()` → `which::which("dotnet").is_ok()`
  - `run(file, _content)`: clippy-shaped — resolve `.cs` → enclosing `.csproj`
    by walking parents; run `dotnet build` with `ErrorLog` into a tmp `.sarif`;
    read it; **C#-private** parse; filter results to `file`. *(Under the Phase-0
    harness this returns empty because the scratch dir has no project — that's
    expected and harmless; the unit tests cover the parser directly.)*
  - **C#-private** `parse_roslyn_sarif` / `roslyn_result_to_diag` /
    `severity_from_str` — copied from kotlin.rs, lenient walk, no schema
    validation, `tool` field hard-coded to `"roslyn"`.
- **Edit** `tool_impls/mod.rs`: `pub mod csharp;` + `pub use csharp::RoslynTool;`.
- **Edit** `core/tool_registry.rs:48`: add `Arc::new(RoslynTool)` to `all_tools`.
- **Edit** `mcp/descriptors.rs:70`: append `csharp` to the `language` doc string.
- **Tests** in `csharp.rs`: parse captured Roslyn SARIF fixtures (a real
  `dotnet build` ErrorLog sample), garbage-tolerance, and file-filtering.

### Phase 1 — Project-scoped, real-disk dispatch (the actual feature)

Make compilation-scoped tools run against the **real project on disk**, once per
`.csproj`, then filter — instead of per-scratch-file.

1. **Plumb `root_path` to the analyze side (cross-crate):**
   - trusty-search: expose `root_path` on the index list/status JSON
     (it already exists on `IndexHandle`).
   - trusty-analyze: add `root_path` to `IndexSummary` / a `get_index_meta`
     call in `core/client.rs`; thread it into `diagnostics_for_index`.
2. **Add a "project-scoped" capability** to `StaticTool` (e.g.
   `fn is_project_scoped(&self) -> bool { false }`, default false; `RoslynTool`
   overrides to true). Or a parallel trait — TBD in the build session.
3. **Branch the dispatch** in `run_diagnostics_blocking`:
   - *file-scoped tools* (today's 10): unchanged scratch-dir path.
   - *project-scoped tools*: skip the scratch reconstruction. Resolve each
     indexed `.cs` to its real path `root_path.join(rel)`, group by enclosing
     `.csproj`, **build each project once** (restore cached), parse the project
     SARIF, and emit diagnostics for every indexed file in that project. This is
     the clippy pattern done right — whole unit, then filter — and amortizes the
     build across all files instead of paying it N times.
4. **Timeout:** give build-class tools an elevated cap (config/env, e.g.
   `TRUSTY_BUILD_TOOL_TIMEOUT_SECS`, default ~300s) instead of the 30s
   `TOOL_TIMEOUT`. Keep restore warm via a stable per-project build dir so
   repeat invocations are incremental.

### Phase 2 — (optional) Generic SARIF ingest

Issue's Option B: an endpoint accepting externally-produced SARIF from any
linter, normalizing into `ToolDiagnostic`. Removes the per-language Rust-adapter
tax — C# (and anything) becomes pure third-party with no in-tree integration.
Cleanest long-term answer; do after Phase 1 proves the normalized output shape.

## 7. Gotchas the build session must handle

- **Comma MUST be `%2C`-escaped — RESOLVED EMPIRICALLY (was O1).** Tested on
  `dotnet 10.0.107` (macOS) against `HotStats.Crypto`:

  | `-p:` form | SARIF produced |
  |---|---|
  | `ErrorLog=out.sarif,version=2.1` (bare comma) | ❌ silently **v1.0.0** (`version` swallowed as a separate property) |
  | `"ErrorLog=out.sarif,version=2.1"` (quoted whole value) | ❌ **no SARIF file** |
  | `ErrorLog=out.sarif%2Cversion=2.1` (**escaped comma**) | ✅ **v2.1.0** |
  | `ErrorLog=out.sarif,version=2` | ❌ v1.0.0 |

  The collision is in MSBuild's `-p:` property parser, **not** the shell, so the
  `%2C` escape is required even when passing the arg as a single argv element
  from Rust. Adapter builds the arg as
  `format!("-p:ErrorLog={path}%2Cversion=2.1")`. Getting this wrong fails
  *silently* (a valid file, wrong version) — the most dangerous failure mode.
- **SARIF version is opt-in.** Roslyn's `ErrorLog` defaults to legacy SARIF v1;
  the literal `version=2.1.0` is rejected (dotnet/roslyn#45644). The emitted
  2.1.0 is the *same* shape detekt uses (`runs[].results[]`, `ruleId`,
  `message.text`, `physicalLocation`, `level`) — **confirmed by inspecting real
  output** (§7.1).
- **`artifactLocation.uri` is a `file://` ABSOLUTE URI — NEW gotcha.** Verified:
  Roslyn emits `file:///Users/.../Crypto.cs`, *not* a relative `A.kt` like
  detekt. The file-filter step (`file_matches`) must strip the `file://` scheme
  and compare absolute paths, not assume a relative tail. This is a concrete
  delta from the detekt parser and another reason to keep them separate.
- **`--no-incremental` is REQUIRED — discovered building HotStats.Crypto.** An
  up-to-date incremental `dotnet build` skips the `CoreCompile` target, so the
  Roslyn analyzers never re-run and the ErrorLog is left empty. Verified: the
  same project that yields **14** diagnostics on a forced build yields **0** on
  a second incremental build. Every adapter invocation must pass
  `--no-incremental` (or delete `obj/`/target `Rebuild`) to force a recompile.
  This is the single biggest performance lever: it means **every** invocation
  pays a full compile, not just cold ones — see §6 Phase 1.4 (the per-`.csproj`
  build dedup + warm-state story is what makes this affordable).
- **Output is native but NOT strictly schema-conformant.** Keep the parser
  lenient (no SARIF-schema validator).
- **Performance is the real risk.** Cold build = restore + full compile, and
  `--no-incremental` forces a full compile *every* time. Design the
  build/cache/latency story deliberately (Phase 1.4): build once per `.csproj`
  (not per file), reuse restore state, and bound concurrency.

### 7.1 Captured real SARIF result (use as the Phase-0 test fixture)

From `dotnet build HotStats.Crypto.csproj --no-restore
-p:ErrorLog=out.sarif%2Cversion=2.1 -p:EnableNETAnalyzers=true
-p:AnalysisLevel=latest-all -p:EnforceCodeStyleInBuild=true` → **14 results**,
8 distinct rules (CA1052, CA1305, CA1507, CA1802, CA2208, CA5379, CA5387,
CA5401). First result, trimmed:

```json
{
  "ruleId": "CA1052",
  "level": "warning",
  "message": { "text": "Type 'Crypto' is a static holder type but is neither static nor NotInheritable" },
  "locations": [{
    "physicalLocation": {
      "artifactLocation": { "uri": "file:///Users/maui/dve/experiments/hotstats/HotStatsGeoAPI/HotStats.Crypto/Crypto.cs" },
      "region": { "startLine": 15, "startColumn": 18, "endLine": 15, "endColumn": 24 }
    }
  }]
}
```

The mapping is 1:1 with `sarif_result_to_diag` in `kotlin.rs` — only the `uri`
normalization differs.

## 8. Signal source — built-in analyzers via flags, no NuGet (validated)

**Default path needs no `PackageReference` edits.** The .NET SDK ships the
NetAnalyzers ruleset; enable it purely through build flags the adapter passes:

```
-p:EnableNETAnalyzers=true -p:AnalysisLevel=latest-all -p:EnforceCodeStyleInBuild=true
```

Validated on `HotStats.Crypto`: this surfaced 14 CA/IDE findings on otherwise
clean-building code, with **zero** project-file modification. This is strictly
better than the issue's "add analyzer NuGets" suggestion for the default path
(no mutation of the user's tree, no restore of extra packages).

*Optional* deeper signal still available by adding `StyleCop.Analyzers` /
`SonarAnalyzer.CSharp` via `Directory.Build.props` — same ErrorLog SARIF output.
**Skip Security Code Scan**: original package deprecated; successor
`SecurityCodeScan.VS2019` stale at 5.6.7 since Sept 2022.

## 8a. Test fixtures (on disk, ready) — a three-tier ladder

`dotnet 10.0.107` is on PATH. Two real C# estates are checked out locally:
**Revecore** (`/Users/maui/dve/portfolio/revecore/repos/...`) and **HotStats**
(`/Users/maui/dve/experiments/hotstats/...`). Use them as a deliberate
difficulty ladder:

**Tier 1 — smoke / parser fidelity: `HotStats.Crypto`** (netstandard2.0, 1
source file). Restores + builds in <1s, no NuGet. Used to validate the ErrorLog
pipeline, capture the §7.1 SARIF fixture, AND validate the shipped Phase-0
`RoslynTool::run()` end-to-end: it returns 14 real CA/CS diagnostics, correctly
scheme-stripped and filtered to the file (PR #942). Good for Phase-0 parser
tests and CI-friendly checks.

**Tier 2 — build-path edge cases: Revecore `ScopeAutomation`** (the largest
**buildable** modern cluster — net8.0 `ScopeAutomationApi` ~60 cs +
`ScopeAutomation.UI.Server` ~55 cs + `ScopeAutomationConsumer` ~17 cs, plus a
`BridgeNg` netcoreapp2.1 ~99 cs). Real multi-project build with project
references and NuGet restore — exercises `.cs` → enclosing `.csproj`
resolution, per-project build dedup, and filtering across several projects.

**Tier 3 — scale, graceful-skip, and the Phase-2 ingest path: Revecore
`Bridge`** (`BottomLineSystems/Bridge`). The real large messy estate they
maintain: **~300 projects, 31 solutions, ~5,600 `.cs` files**, of which
**299/300 are legacy non-SDK**, 283 target **.NET Framework v4.8**, 286 use
`packages.config`; WinForms + classic `.asmx` services, including a
**17,101-line** `EDISearchForm.cs`; mixed TFMs in one tree (v4.8/v4.6.1/v4.0/
v4.5/v3.5/net48). **Bridge is NOT buildable by the in-tree `dotnet build`
path** — but the reason is its *legacy non-SDK + `packages.config` format* plus
Windows-only frameworks (WinForms/WPF/`System.Web`/`.asmx`), **not** the `net48`
target itself (§8c). It is therefore the canonical test for the two hardest
behaviours:
- **graceful-skip** — the adapter must detect unbuildable projects (legacy
  non-SDK / `packages.config`; Windows-only-framework projects) and no-op per
  project, never erroring the whole `run_diagnostics` call or hanging on a
  doomed restore;
- **Phase 2 (generic SARIF ingest)** — the *only* way to lint an estate like
  Bridge: build it on Windows in the user's own VS/MSBuild (same ErrorLog SARIF,
  or Roslynator/VS), then POST the SARIF. Bridge is the proof that Phase 2 is
  not optional polish but the path for the common real-world shape.

**Estate-wide reality (both repos):** **584 `.csproj`, only 71 SDK-style, 513
legacy non-SDK.** The build-on-Linux line is **project *format*, not target
framework** (see §8c — verified empirically): `dotnet build` on macOS/Linux
builds **any SDK-style project, including `net48`/`net472`** (the SDK
auto-restores the Framework reference assemblies), but cannot drive **legacy
non-SDK / `packages.config`** projects. So the buildable subset is the ~71
SDK-style projects (regardless of TFM); the 513 legacy non-SDK projects are the
ones that need graceful-skip + Phase 2. The legacy-heavy skew is exactly why
those are first-class requirements, not afterthoughts.

**Edge cases Bridge specifically surfaces** (for whoever hardens the adapter):
mixed-TFM solutions; `<TargetFramework>` vs legacy `<TargetFrameworkVersion>`;
multi-targeting (`net8.0;net48;net472`); `packages.config` (no SDK restore);
generated files (`*.Designer.cs`, `*.asmx.cs`) that should likely be filtered;
and very large single files that stress whole-file handling and SARIF volume.

## 8c. .NET Framework on Linux — what builds, what doesn't (verified)

"Can you lint .NET Framework on Linux?" is commonly answered "no" because the
*runtime* is Windows-only. That's the wrong frame: **Roslyn and the C# compiler
are fully cross-platform, so `net4x` *compiles* (and therefore lints) on
Linux/macOS.** The blocker is three *separable* things, none of which is "the
Framework target":

1. **Project format** — legacy non-SDK (`<Project ToolsVersion="15.0"
   xmlns="…msbuild/2003">`) vs SDK-style. The .NET SDK's MSBuild drives
   SDK-style cleanly; legacy projects don't get the implicit reference-assembly
   package and don't restore well.
2. **`packages.config` restore** — `dotnet restore` only handles
   `PackageReference`; it *silently no-ops* on `packages.config` (needs classic
   `nuget.exe` / Mono).
3. **Windows-only frameworks** — WinForms, WPF, classic `System.Web` / `.asmx`.
   These assemblies have no cross-platform build, regardless of project format.

**Empirically verified on this host (`dotnet 10.0.107`, macOS, no Mono):**

| Project shape | Result |
|---|---|
| **SDK-style, `net48`, `PackageReference`** (`HotStats.ExcelExport`, `-f net48`) | ✅ **built + 7 analyzer diagnostics**; SDK auto-restored the Framework reference assemblies |
| **Legacy non-SDK, `packages.config`, `net48`** (`Bridge/.../OklahomaHeart`) | ❌ `MSB3644: reference assemblies for .NETFramework v4.8 not found`; `dotnet restore` reported "none of the projects contain packages to restore" (ignored `packages.config`) |

**Implication for the adapter:** RoslynTool **already lints any SDK-style
project on Linux/macOS, `net48`/`net472` included** — that subset needs no
special handling. Only legacy non-SDK / `packages.config` / Windows-only-
framework projects fall to graceful-skip + Phase 2.

**Partial fallback for legacy *non-UI* libraries — `FrameworkPathOverride`.**
The `MSB3644` failure above is *only* the missing reference assemblies. You can
supply them cross-platform without Windows: restore the
`Microsoft.NETFramework.ReferenceAssemblies.net48` NuGet once, then build the
legacy project with
`dotnet build /p:FrameworkPathOverride=<…/lib/net48>` (or set
`FrameworkPathOverride` in a repo-root `Directory.Build.props`). This clears
`MSB3644` and lets Roslyn analyzers run on legacy non-SDK class libraries. It
does **not** solve blockers #2 (`packages.config` dependencies stay unresolved →
`CS0246` noise for types from those packages) or #3 (WinForms/WPF/`System.Web`
have no cross-platform assemblies). So it's a *best-effort* path for pure-BCL /
non-UI legacy libs, not a general fix — Bridge's WinForms/`.asmx` projects still
require a Windows build → SARIF ingest (Phase 2).

**Decision tree for Phase 1/2:**
- SDK-style (any TFM) → build in-tree today (Phase 1).
- Legacy non-SDK, non-UI, BCL-only deps → optional `FrameworkPathOverride`
  best-effort (degraded semantics where `packages.config` deps are referenced).
- Legacy non-SDK with `packages.config` deps and/or WinForms/WPF/`System.Web` →
  Windows build + generic SARIF ingest (Phase 2). This is the Bridge case.

## 9. Open questions to resolve during the build

- **O1 — MSBuild comma-escaping: RESOLVED.** Use `%2C`
  (`-p:ErrorLog=path%2Cversion=2.1`). See §7. Bare comma silently yields v1.0.0.
- **O2 — Warm-build latency:** MSBuild-server / long-lived workspace / BuildHost
  approach to avoid repeated restore + cold compile. Decides whether C# linting
  is practically usable at scale.
- **O3 — `StaticTool` extension shape:** add `is_project_scoped()` to the
  existing trait vs. a new `ProjectScopedTool` trait. Affects the dispatch
  branch in §6 Phase 1.3.
- **O4 — SonarAnalyzer.CSharp / sonar-scanner** standalone SARIF on Linux
  without a SonarQube server — unconfirmed; treat as analyzer-NuGet-only.
- **O5 — Roslyn conformance deltas:** does the C# parser need a normalization
  shim beyond the lenient walk?

## 10. Acceptance criteria

- `run_diagnostics` with a `dotnet`-equipped host returns Roslyn diagnostics for
  `.cs` files in an indexed real-disk .NET project, correctly filtered per file.
- No regression to the existing 10 file-scoped adapters.
- `RoslynTool` never panics; absent `dotnet` → silently skipped at discovery.
- C#-private SARIF parser unit-tested against the captured §7.1 fixture,
  incl. `file://`-URI normalization in the file-filter and garbage tolerance.
- `descriptors.rs` advertises `csharp`; registry discovers `RoslynTool`.
- Build-class timeout is configurable and defaults high enough for a cold build.
- Adapter passes `-p:EnableNETAnalyzers=true -p:AnalysisLevel=latest-all
  -p:EnforceCodeStyleInBuild=true` and `%2C`-escapes the ErrorLog comma.
- Unbuildable projects (legacy non-SDK, Framework-only TFMs off-Windows) are
  skipped gracefully, not surfaced as errors.

## 11. Key sources

- Roslyn ErrorLog format: https://github.com/dotnet/roslyn/blob/main/docs/compilers/Error%20Log%20Format.md
- `errorlog` option (version values): https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/compiler-options/errors-warnings
- SARIF v1-default history: https://github.com/dotnet/roslyn/issues/26538 · version-string rejection: https://github.com/dotnet/roslyn/issues/45644
- Compilation/SemanticModel scoping: https://learn.microsoft.com/en-us/dotnet/csharp/roslyn-sdk/get-started/semantic-analysis
- Roslynator analyze (XML/GitLab only): https://josefpihrt.github.io/docs/roslynator/cli/commands/analyze
- Security Code Scan staleness: https://www.nuget.org/packages/SecurityCodeScan
