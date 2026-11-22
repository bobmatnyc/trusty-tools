# Multi-Repository Code Quality Analysis System

**Date**: 2026-04-23
**Scope**: Design research for a system to clone/scan multiple Git repos, run static analysis, generate comparative quality reports, identify highest-risk files, and output summary tables + detailed JSON.

---

## 1. Language Recommendation: Python + Rust Hybrid

**Primary implementation: Python**

Python is the right choice for the analysis layer because:
- All major static analysis tools (radon, lizard, pylint, coverage.py) have Python APIs
- `subprocess` + `gitpython` make repo cloning/scanning straightforward
- The project already has `pyproject.toml` and a `git_analyzer` Python package with reusable patterns
- JSON/table output is trivial with stdlib + tabulate/rich

**Rust component: optional, for indexing**

The existing `src/search/indexer.rs` (tree-sitter-based AST chunker supporting Rust, Python, JS, Go) could be invoked via `cargo run -- --reindex` on each cloned repo to produce vector-searchable code chunks. This is additive, not required for MVP.

---

## 2. What Already Exists (Reusable)

### Python: `git_analyzer/` package
- `/Users/masa/Projects/open-mpm/git_analyzer/src/git_analyzer/metrics.py` — `AnalysisResult`, `AuthorStats`, `BusFactor`, `CommitPatterns` dataclasses with `.to_dict()` serialization. Directly reusable for per-repo git churn metrics (high-churn files = risk signal).
- `parser.py` — git log parsing
- `reporter.py` — report formatting

### Python: `table_formatter/` package
- `/Users/masa/Projects/open-mpm/table_formatter/src/table_formatter/` — `formatter.py`, `reader.py`, `filters.py`. Reusable for rendering comparative markdown tables.

### Rust: `src/search/indexer.rs`
- AST-aware chunker via tree-sitter for Rust, Python, JS, Go files
- `CodeChunk { file, function_name, start_line, end_line, language, score, text }`
- Supports `index_directory` and `search` against redb+usearch store
- Could provide per-function complexity signals if extended

### Rust: `src/search/watcher.rs` + `redb_usearch.rs`
- Incremental file watching + vector embedding store — useful if adding semantic similarity across repos

---

## 3. Static Analysis Tools by Language

| Language | Complexity | Duplication | Coverage | Notes |
|---|---|---|---|---|
| Python | `radon` (cyclomatic/halstead/MI), `wily` | `pylint --duplicate-code`, `jscpd` | `coverage.py` + `pytest-cov` | radon has CLI + Python API |
| JavaScript/TypeScript | `escomplex` / `ts-complex`, `plato` | `jscpd` | `c8`, `nyc` | jscpd is language-agnostic |
| Rust | `cargo clippy` (lints), `cargo-llvm-cov` | `tokei` (LOC) | `cargo-llvm-cov`, `cargo-tarpaulin` | No direct cyclomatic tool; clippy cognitive complexity lint |
| Go | `gocyclo`, `gocognit` | `dupl` | `go test -coverprofile` | |
| Universal | `lizard` | `jscpd` | — | lizard supports C/C++/Java/Python/JS/Go/Rust via regex |

**Recommended core**: `lizard` (pip installable, supports 10+ languages, outputs JSON, computes cyclomatic complexity and token count per function) + `jscpd` (duplication, JSON output) + `git_analyzer` (churn-based risk).

---

## 4. Verified Library APIs (tested 2026-04-23)

### radon — Python complexity + metrics

**Install**: `pip install radon`

```python
from radon.complexity import cc_visit, cc_rank
from radon.metrics import mi_visit, h_visit
from radon.raw import analyze

# Cyclomatic Complexity — returns list of Function/Method/Class objects
results = cc_visit(source_code)
for r in results:
    # r.name, r.complexity (int), r.lineno, r.endline, r.classname (or None)
    rank = cc_rank(r.complexity)  # 'A'–'F'

# Maintainability Index — single float (0–100, higher = more maintainable)
mi = mi_visit(source_code, multi=True)  # multi=True counts multiline strings

# Raw metrics — returns namedtuple with: loc, lloc, sloc, comments, multi, blank, single_comments
raw = analyze(source_code)
# raw.loc, raw.sloc, raw.comments

# Halstead metrics — returns Halstead(total=HalsteadReport(...), functions=[(name, HalsteadReport), ...])
h = h_visit(source_code)
# h.total.volume, h.total.difficulty, h.total.effort, h.total.bugs
# h.functions is list of (fn_name, HalsteadReport) tuples
# HalsteadReport fields: h1, h2, N1, N2, vocabulary, length, calculated_length,
#                        volume, difficulty, effort, time, bugs
```

**Gotcha**: `HalsteadVisitor.from_code()` does NOT expose h1/h2 directly — use `h_visit()` instead.

**Gotcha**: `mi_visit()` returns `nan` on empty files — guard with `if source.strip()`.

**CC rank scale**: A=1-5 (simple), B=6-10, C=11-15, D=16-20, E=21-25, F=26+ (untestable)

### lizard — multi-language complexity (C/C++/Java/Python/JS/Go/Rust/Swift/...)

**Install**: `pip install lizard`

```python
import lizard

# Analyze a single file — returns FileInformation
fi = lizard.analyze_file(path_str)
# fi.average_cyclomatic_complexity  (float)
# fi.nloc                           (int, non-comment lines)
# fi.token_count                    (int)
# fi.function_list                  list of FunctionInfo

for fn in fi.function_list:
    # fn.name, fn.cyclomatic_complexity, fn.nloc, fn.length
    # fn.parameters (list of str), fn.start_line, fn.end_line
    # fn.filename
    pass

# Bulk scan — iterate over all source files in a directory
for fi in lizard.analyze_files(lizard.get_all_source_files(repo_dir)):
    ...
```

**Gotcha**: `max_nesting_depth` is NOT available on `FunctionInfo` in lizard 1.20. Use `--CCN` threshold flag or filter by `cyclomatic_complexity`.

**Gotcha**: `lizard.get_all_source_files(path)` walks recursively but follows symlinks — be careful with cloned repos that have symlinked node_modules.

### flake8 — Python linting score

No stable programmatic Python API. Use subprocess with `--format=default`:

```python
import subprocess, pathlib

def run_flake8(repo_path: str) -> list[dict]:
    result = subprocess.run(
        ["python", "-m", "flake8", "--format=default",
         "--max-line-length=100", "--extend-ignore=E501", repo_path],
        capture_output=True, text=True
    )
    violations = []
    for line in result.stdout.strip().splitlines():
        # format: /path/to/file.py:LINE:COL: CODE message
        parts = line.split(":", 3)
        if len(parts) >= 4:
            violations.append({
                "file": parts[0], "line": int(parts[1]),
                "col": int(parts[2]), "msg": parts[3].strip()
            })
    return violations
```

**Gotcha**: flake8 exits with code 1 if ANY violation found, and code 0 only if clean — do not use `check=True`.

**Gotcha**: `--format=json` is NOT a built-in format; requires `flake8-json` plugin.

### GitPython — repo cloning and traversal

**Install**: `pip install gitpython`

```python
import git, tempfile, pathlib

def acquire_repo(source: str, dest: str | None = None) -> git.Repo:
    if pathlib.Path(source).exists():
        return git.Repo(source)
    dest = dest or tempfile.mkdtemp()
    # depth=1 for shallow clone — much faster, use multi_options for git flags
    repo = git.Repo.clone_from(
        source, dest,
        multi_options=["--depth=1", "--single-branch"]
    )
    return repo

# Walk all files via tree traversal
repo = git.Repo(path)
files = [item.path for item in repo.head.commit.tree.traverse()
         if item.type == "blob"]

# Get churn — number of commits touching a file (requires full clone)
file_commits = list(repo.iter_commits(paths="src/foo.py"))
churn = len(file_commits)
```

**Gotcha**: `multi_options` takes a list of individual git flags as strings, not a single concatenated string.

**Gotcha**: `clone_from` requires `allow_unsafe_protocols=True` for non-https/git URLs (e.g., file:// or custom).

**Gotcha**: Churn metrics require full clone (`--depth=1` omits history). For churn, use `git log --format="%H" -- <file>` via subprocess if shallow clone is preferred.

### rich — terminal table output

```python
from rich.console import Console
from rich.table import Table

console = Console()
table = Table(title="Code Quality Report")
table.add_column("Repository", style="cyan", no_wrap=True)
table.add_column("Avg CC", justify="right")
table.add_column("MI Score", justify="right")
table.add_column("Violations/KLOC", justify="right")
table.add_column("Risk", style="bold red")

for repo in sorted_repos:
    risk_color = "red" if repo.risk > 0.7 else "yellow" if repo.risk > 0.4 else "green"
    table.add_row(
        repo.name, f"{repo.avg_cc:.1f}", f"{repo.mi:.1f}",
        f"{repo.violations_per_kloc:.0f}",
        f"[{risk_color}]{repo.risk_label}[/{risk_color}]"
    )
console.print(table)
```

---

## 5. Comparative Scoring Pattern

```python
from dataclasses import dataclass
from typing import NamedTuple

@dataclass
class RepoMetrics:
    name: str
    avg_cc: float          # radon/lizard — lower is better
    max_cc: int            # worst function
    mi_score: float        # radon MI — higher is better (0-100)
    violations_per_kloc: float  # flake8 violations / (sloc / 1000)
    duplication_pct: float # jscpd % — lower is better
    churn_p90: int         # p90 commits per file (needs full clone)

def normalize_to_risk(metrics: list[RepoMetrics]) -> dict[str, float]:
    """Normalize each metric to [0, 1] risk contribution (1 = worst)."""
    def norm(values, invert=False):
        lo, hi = min(values), max(values)
        if hi == lo:
            return [0.5] * len(values)
        normed = [(v - lo) / (hi - lo) for v in values]
        return [1 - n for n in normed] if invert else normed

    avg_cc_norm = norm([m.avg_cc for m in metrics])
    mi_norm     = norm([m.mi_score for m in metrics], invert=True)  # high MI = low risk
    viol_norm   = norm([m.violations_per_kloc for m in metrics])
    dup_norm    = norm([m.duplication_pct for m in metrics])

    WEIGHTS = {"cc": 0.35, "mi": 0.25, "viol": 0.25, "dup": 0.15}
    scores = {}
    for i, m in enumerate(metrics):
        scores[m.name] = (
            WEIGHTS["cc"]   * avg_cc_norm[i] +
            WEIGHTS["mi"]   * mi_norm[i] +
            WEIGHTS["viol"] * viol_norm[i] +
            WEIGHTS["dup"]  * dup_norm[i]
        )
    return scores  # 0 = cleanest, 1 = riskiest
```

### Risk Score Composition
```
risk_score(file) = 0.35 * norm(avg_cc) + 0.25 * norm(1/mi) + 0.25 * norm(violations/kloc) + 0.15 * norm(duplication_pct)
```
Normalize each metric to [0, 1] across all repos before weighting.

### Output Structure
```json
{
  "repos": [
    {
      "url": "...",
      "metrics": { "avg_complexity": 8.2, "max_complexity": 42, "duplication_pct": 12.1, "files_scanned": 87 },
      "risk_score": 0.71,
      "highest_risk_files": [{"file": "...", "complexity": 42, "churn": 19, "risk": 0.91}]
    }
  ],
  "comparative_table": "...",
  "generated_at": "2026-04-23T..."
}
```

---

## 5. Agent Integration Path (open-mpm workflow)

The system maps cleanly onto existing agent roles:
- `research-agent`: fetch repo metadata, identify language stack
- `local-ops-agent`: `git clone`, run `lizard`/`jscpd` via `ShellExecTool`
- `code-agent`: generate the Python analysis script
- `observe-agent`: synthesize comparative table + JSON report

Alternatively, implement as a standalone Python CLI (`python -m quality_analyzer <repo_urls...>`) that the `qa-agent` can invoke via `ShellExecTool`.

---

## 6. Files to Read for Implementation Start

- `/Users/masa/Projects/open-mpm/git_analyzer/src/git_analyzer/metrics.py` — reuse `AnalysisResult`
- `/Users/masa/Projects/open-mpm/git_analyzer/src/git_analyzer/reporter.py` — reuse report rendering
- `/Users/masa/Projects/open-mpm/table_formatter/src/table_formatter/formatter.py` — reuse table output
- `/Users/masa/Projects/open-mpm/src/search/indexer.rs` — optional Rust AST chunking integration

---

## 7. Supplementary Research — 2026-04-24

### Cognitive Complexity Gap

lizard computes cyclomatic complexity only. For cognitive complexity (which penalizes deeply nested control flow more accurately than cyclomatic):
- Python only: `radon` v6+ added `cc_visit` with `cognitive_complexity` attribute on `Function` objects
- JavaScript only: `eslint-plugin-sonarjs` rule `cognitive-complexity` (requires Node.js, not Python-callable without subprocess)
- Cross-language: No pip-installable library. The closest is SonarQube/SonarCloud (server-based, not embeddable)
- Practical recommendation: use lizard cyclomatic as the primary cross-language metric; supplement with radon cognitive for Python files only

### Duplication — Dependency Trade-offs

| Tool | Language support | Python-native | Notes |
|---|---|---|---|
| `lizard --duplicate` | 27+ languages | Yes | Token-based, fast, no extra deps |
| `jscpd` (npm) | 150+ languages | Via subprocess | Requires Node.js; best quality |
| PMD CPD (via `cpd` pip) | 20+ languages | Via subprocess | Requires Java 11+ |

For zero external runtime dependency: use `lizard --duplicate` (flag: `lizard -l all --duplicate <dir>`). Output is text-only; parse with regex.

### Coverage Report Detection — Priority Order

Scan repo root and common subdirectories for these files in priority order:

1. `coverage.xml` (Cobertura — Python, Java, C#, JS via jest)
2. `lcov.info` or `lcov/lcov.info` (Rust tarpaulin, C/C++, Go)
3. `.coverage` (Python coverage.py binary — readable via `coverage.Coverage(data_file=...)`)
4. `coverage-summary.json` (Istanbul/nyc for JavaScript)
5. `target/llvm-cov/lcov.info` (Rust cargo-llvm-cov)

If none found, emit `"coverage": null` in output — do not fabricate.

### GitPython — Shallow Clone and Parallelism

```python
from concurrent.futures import ThreadPoolExecutor
import git, tempfile, pathlib

def clone_or_open(url_or_path: str) -> tuple[git.Repo, str]:
    p = pathlib.Path(url_or_path)
    if p.exists():
        return git.Repo(url_or_path), str(p)
    dest = tempfile.mkdtemp()
    repo = git.Repo.clone_from(
        url_or_path, dest,
        multi_options=["--depth=1", "--single-branch"]
    )
    return repo, dest

# Parallel cloning — ThreadPoolExecutor safe for GitPython (I/O-bound)
with ThreadPoolExecutor(max_workers=4) as pool:
    futures = {pool.submit(clone_or_open, url): url for url in repo_urls}
```

Do NOT use `ProcessPoolExecutor` with GitPython — git subprocess handles do not serialize across process boundaries.

### Rich vs Tabulate — When to Use Each

| Scenario | Use |
|---|---|
| Human-readable terminal output with color | `rich.table.Table` |
| Markdown/GitHub table output | `tabulate(data, tablefmt="github")` |
| Pipe output to another process | `tabulate` or plain JSON |
| Progress bars during scan | `rich.progress.Progress` |

Pattern for dual output (human + JSON):

```python
import sys, json
from rich.console import Console
from rich.table import Table

def output(repos: list[dict], fmt: str = "table"):
    if fmt == "json":
        json.dump({"repos": repos}, sys.stdout, indent=2)
        return
    console = Console()
    table = Table(title="Code Quality Report")
    # ... add columns and rows
    console.print(table)
```

Detect `--format json` at CLI entry point; never mix rich output with JSON stdout.
