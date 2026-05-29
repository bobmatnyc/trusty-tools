# ADR 0002 — Performance Hotspots in the Collection Pipeline

Status: Accepted
Date: 2026-05-11
Related: Issue #50 ("Profiling: known hotspot elimination")

## Context

The Rust port targets the same semantics as the Python `gitflow-analytics`
predecessor but with substantially better throughput. Early profiling on a
58K-commit monolith identified three suspected hotspots:

1. **libgit2 tree walks** in `compute_commit_diff` — every commit triggers
   two ODB lookups (`commit.tree()` and `commit.parent(0)?.tree()`), each
   of which can dominate when ODBs are large or cold.
2. **Rayon + SQLite write contention** — if commit rows were inserted from
   multiple rayon workers, the `rusqlite::Connection` (which is not
   `Sync`) would force per-worker connections, all contending for the
   single database writer lock under WAL.
3. **JSON deserialization** in the GitHub / JIRA HTTP clients — double
   deserialization (into `serde_json::Value`, then into a typed struct) is
   a common antipattern that can double JSON parse cost.

This ADR records the findings from the audit and the fixes applied (or
not applied, with rationale).

## Findings

### 1. Rayon SQLite write contention — **no fix required**

`src/collect/collector.rs` and `src/collect/git/extractor.rs` already write
commits to the database in a **single serial transaction** on the main
thread. The relevant code in `GitCollector::collect_window`:

```rust
let tx = db.connection_mut().transaction()?;
for oid_res in revwalk {
    // ... extract commit ...
    tx.execute("INSERT OR IGNORE INTO commits ...", params![...])?;
    for f in &diff.files {
        tx.execute("INSERT INTO files ...", params![...])?;
    }
}
tx.commit()?;
```

There is no `rayon::par_iter()` in the write path. Diff computation is
serial as well; per-commit diffs are independent and could be parallelised
in the future, but the rows would still need to be collected into a `Vec`
and inserted serially under a single transaction. Marking as
**no-op — already correct pattern**.

### 2. JSON deserialization — **no fix required**

Audited `src/collect/github/client.rs` and `src/collect/jira/client.rs`.
All response deserialisation already uses a single `resp.json::<T>().await?`
call directly into the typed target struct:

```rust
// github/client.rs
let pulls: Vec<ApiPull> = resp.json().await?;
let issue: GitHubIssue = resp.json().await?;
let batch: Vec<GitHubReview> = resp.json().await?;
let batch: Vec<GitHubPrCommit> = resp.json().await?;
let batch: Vec<GitHubIssue> = resp.json().await?;

// jira/client.rs
let issue: ApiIssue = resp.json().await?;
let parsed: SearchResponse = resp.json().await?;
let fields: Vec<FieldDescriptor> = resp.json().await?;
```

No intermediate `serde_json::Value` parse, no `from_str` double-decode.
Marking as **no-op — already optimal**.

### 3. libgit2 tree walks — **profiling note added; deeper optimisation deferred**

`compute_commit_diff` calls:

- `commit.tree()` — ODB lookup for the commit's root tree
- `commit.parent(0)?.tree()` — ODB lookup for the parent commit, then
  ODB lookup for *its* root tree

On the reference 58K-commit monolith, profiling shows ~35% of
`collect_window` wall time spent in these two object loads. Two
optimisations were considered:

#### A. Parent-tree caching across adjacent revwalk steps

`revwalk` is configured with `Sort::TIME`, which yields commits in
descending author-time order. This does NOT guarantee that
`commit[i].parent(0) == commit[i+1]` — particularly on merge-heavy
histories where time ordering and ancestry ordering diverge. A cache of
the previous commit's tree would have a workload-dependent hit rate
ranging from near-zero (heavy merges, parallel feature branches) to near-
100% (linear histories).

**Decision**: deferred. The cache adds non-trivial code and a fallback
ODB lookup is still required for the miss path, so the benefit only
appears on workloads that already complete reasonably quickly. A profiling
note has been added inline in `src/collect/git/diff.rs` so a future
contributor working on this hotspot has the context.

#### B. Skip rename detection for low-churn commits

`diff.find_similar(...)` with rename/copy detection enabled walks the
diff a second time. For commits that produce few add/delete pairs this
is wasted work. Disabling it conditionally on `stats.files_changed() < N`
would save measurable time.

**Decision**: deferred. Rename detection is semantically required for
correct line-count attribution (`feat: rename module` should not produce
`+200 / −200`). Adding a heuristic threshold risks regressing report
accuracy.

## Decision

- Add an explicit profiling note in `compute_commit_diff` documenting the
  hotspot and the two deferred optimisations.
- No changes to the collector write path (already serial, single
  transaction).
- No changes to the GitHub / JIRA HTTP clients (already single-step
  deserialization).
- Add a Criterion benchmark suite (`benches/tga_bench.rs`) covering five
  hot paths so future regressions can be caught quantitatively. See
  issue #47.

## Consequences

- The collection pipeline retains a well-understood shape: single-threaded
  walk, single transaction, single-step JSON parse. Any future "this is
  slow" report can be diff'd against the bench baseline.
- The libgit2 hotspot is documented in code and in this ADR, with two
  optimisation paths sketched out for a future contributor to pick up.

## References

- `src/collect/git/extractor.rs::collect_window`
- `src/collect/git/diff.rs::compute_commit_diff`
- `src/collect/github/client.rs`
- `src/collect/jira/client.rs`
- `benches/tga_bench.rs`
