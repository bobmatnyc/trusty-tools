# ADR 0003: Bitbucket Cloud PR Provider

- **Status**: Accepted
- **Date**: 2026-05-12
- **Deciders**: trusty-git-analytics core team
- **Related**: Issue #71 (`store_pull_requests` non-UNIQUE index bug)

## Context

Pull-request analytics were GitHub-only at the start of this work. The
GitHub client lived at `src/collect/github/client.rs` and was instantiated
directly from `CollectionPipeline::run`:

```rust
if let Some(gh_cfg) = &self.config.github {
    if gh_cfg.fetch_prs {
        match GitHubClient::new(gh_cfg) { ... gh.fetch_pull_requests() ... }
    }
}
```

No trait, no factory, no URL parsing. A user with their code on Bitbucket
Cloud could not run any PR-driven metric — cycle time, PR author counts,
merge-commit attribution — even though the rest of the pipeline (git
extraction, identity resolution, classification) is provider-agnostic.

The Bitbucket Cloud REST API is similar enough to GitHub's that a shared
abstraction is reasonable, but the differences are not cosmetic:

- **Pagination** is cursor-driven (`next: <absolute-url>`), not
  page-counter driven.
- **Auth** has two modes — Bearer access token *or* Basic auth with an
  App Password — and Atlassian is migrating away from the latter.
- **PR shape** differs in non-trivial ways: `id` vs `number`, an `author`
  object with optional `display_name`/`nickname`/`uuid` vs a flat `login`,
  and four states (`OPEN`, `MERGED`, `DECLINED`, `SUPERSEDED`) vs three.
- The PR list endpoint **excludes merged history by default** —
  `state=OPEN` is the implicit filter unless overridden.
- Per-PR commits require a **second API call** (`/pullrequests/{id}/commits`).

A `PmAdapter` trait already exists for tickets (Jira/Linear/ADO/GitHub
Issues). It was tempting to broaden it to cover PRs as well, but the two
domains differ in important ways: PR providers fetch the entire
repository's PR history in one bulk call, while PM adapters look up
specific tickets by id and never enumerate. Their lifecycles, error
modes, and persistence schemas have nothing in common.

We also considered whether to support Bitbucket Server / Data Center in
the same PR. The decision was no — its REST API surface is essentially
disjoint from Bitbucket Cloud's (`/rest/api/1.0/...` vs
`/2.0/repositories/...`), and conflating them under one client would
produce a config that lies about which fields apply.

## Decision

### 1. A separate `PrProvider` trait

New trait at `src/collect/pr_provider.rs`:

```rust
#[async_trait]
pub trait PrProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>>;
    fn store_pull_requests(&self, db: &Database, prs: &[PullRequest])
        -> crate::core::Result<usize>;
}
```

- Implemented by both `GitHubClient` and the new `BitbucketClient`.
- Uses `#[async_trait]` (already a project dep, also used by `PmAdapter`)
  so the trait is `dyn`-compatible — we can store
  `Vec<Box<dyn PrProvider + Send + Sync>>` and iterate.
- `store_pull_requests` is **synchronous** because `rusqlite::Connection`
  is synchronous; keeping that on the trait avoids an awkward async-over-
  blocking layer.
- `name()` is for logs and error attribution only — never used for control
  flow.

`PmAdapter` is left alone. The two traits live side-by-side in
`src/collect/`.

### 2. Bitbucket Cloud only

The new client at `src/collect/bitbucket/` targets Bitbucket Cloud REST
v2.0 against `https://api.bitbucket.org/2.0`. Bitbucket Server is out of
scope and will get its own provider if/when needed — likely a third
`PrProvider` impl rather than a kind-flag on this one.

### 3. Cursor pagination, all states

The PR list call uses an explicit state filter and follows `next`:

```rust
let initial = format!(
    "{}/repositories/{}/{}/pullrequests\
     ?state=OPEN&state=MERGED&state=DECLINED&state=SUPERSEDED\
     &pagelen={PAGE_SIZE}",
    self.api_base, self.workspace, self.repo_slug
);
let mut next_url: Option<String> = Some(initial);
while let Some(url) = next_url.take() {
    let page: BbPaged<BbPullRequest> = self.retry_request(&url).await?
        .error_for_status()?.json().await?;
    // map + extend
    next_url = page.next;
}
```

The repeated `state=` query params are how Bitbucket spells "OR" across
states. Without this, the default `OPEN`-only filter would silently drop
all merged history — a result that looks correct in spot checks and
would corrode every downstream metric.

`pagelen` is capped at 50 (Bitbucket's documented maximum).

### 4. Auth: Bearer wins, App Password is fallback

Two modes supported, checked in this order:

1. **Bearer token** — `bitbucket.token` in YAML, or `BITBUCKET_TOKEN` env.
2. **Basic auth** — `bitbucket.username` + `bitbucket.app_password` (or
   `BITBUCKET_APP_PASSWORD` env).

The validator enforces "at least one mode populated when `fetch_prs:
true`". Bearer wins when both are set because workspace / repository
access tokens are Atlassian's recommended replacement for App Passwords
and we want the migration path to be a no-op.

The `BitbucketConfig` field names (`token`, `username`, `app_password`)
are chosen so that an eventual rename to "API Token" only requires a doc
update, not a schema change.

### 5. State mapping is lossy

The shared `pull_requests.state` column has three values: `open`,
`closed`, `merged`. Bitbucket's four states collapse as:

| Bitbucket | Stored |
|-----------|--------|
| `OPEN`    | `open` |
| `MERGED`  | `merged` |
| `DECLINED`  | `closed` |
| `SUPERSEDED` | `closed` |

`DECLINED` and `SUPERSEDED` are distinguishable on the wire but indistinguishable
in the database after this collapse. We accept the loss for v1 because:

- No current report differentiates "abandoned" from "superseded by another PR".
- Adding `Declined` / `Superseded` to `PrState` is a wider blast radius —
  every existing `match` on the enum becomes non-exhaustive, every report
  has to decide how to render the new states.
- The Bitbucket payload is preserved nowhere, so if a future report
  needs the distinction we will have to either (a) refetch, or (b) add
  a `raw_state TEXT` column. This is recorded so future-us can find it.

### 6. `merged_at` is sourced from `updated_on`

Bitbucket Cloud's `/pullrequests` list endpoint does not surface a dedicated
`merged_on` field. The only timestamp on a merged PR row is `updated_on`,
which reflects the most recent edit to the PR — including post-merge edits
to title, description, or reviewer list. The Bitbucket client therefore
records `merged_at = updated_on` for `state == MERGED`, accepting that the
recorded time can drift later than the actual merge moment if the PR is
edited after merging.

Consequences worth knowing:

- DORA cycle-time and lead-time metrics computed from `merged_at - created_at`
  will be **biased upward** for Bitbucket PRs that get edited post-merge.
  GitHub PRs are unaffected (GitHub returns a real `merged_at`).
- The error is bounded in practice — most PRs are not edited after merge —
  and goes in only one direction (later, never earlier).
- A merged PR that has never been edited has `updated_on == merged_on` and
  the recorded value is exact.

This is recorded so a future report consumer can decide whether to (a) ignore
the bias, (b) refetch with the activity endpoint to find the real merge
event, or (c) add a `merged_at_source TEXT` column. Today no downstream
report distinguishes the two providers when computing cycle time, so the
bias is silent. The `pull_requests` row remains internally consistent —
`state == merged` always implies `merged_at IS NOT NULL` because the live
integration test asserts it.

### 7. One commit SHA per PR (no per-PR commits fetch)

GitHub's client stores only the merge commit hash in `commit_shas`
(JSON array of one element, or `[]` for unmerged PRs). The Bitbucket
client matches that contract by reading `merge_commit.hash` directly
from the PR list payload. The dedicated `/pullrequests/{id}/commits`
endpoint exists and would yield the full series-of-commits, but calling
it would add one round trip per merged PR — on a 5000-PR repo that's
5000 extra HTTPS calls against a 1000 req/hr/user quota.

If a future report needs full PR commit lists, the right move is to add
an opt-in `bitbucket.fetch_commits: bool` (mirrored on the GitHub side)
rather than enabling it globally. Recorded as a known limit, not a bug.

### 8. Concurrent providers via `tokio::task::JoinSet`

`CollectionPipeline::fetch_and_store_prs` builds
`Vec<Arc<dyn PrProvider + Send + Sync>>` from config, spawns one task
per provider on a `JoinSet`, and drains results on the main task:

```rust
let mut set: tokio::task::JoinSet<(String, Result<Vec<PullRequest>>)>
    = tokio::task::JoinSet::new();
for p in &providers {
    let p = Arc::clone(p);
    let name = p.name().to_string();
    set.spawn(async move { (name, p.fetch_pull_requests().await) });
}
while let Some(joined) = set.join_next().await {
    // match provider by name, call store_pull_requests on the main task
}
```

- `JoinSet` was chosen over `futures::future::join_all` to avoid adding
  the `futures` crate as a direct dependency.
- **Persistence runs on the main task**, not inside the spawned future.
  `rusqlite::Connection` is `Send` but not `Sync`, and the orchestrator
  holds `&mut Database` for the entire pipeline. Pushing storage into
  the provider tasks would require either an `Arc<Mutex<Database>>` or
  per-task connections — both unjustified for two providers serialized
  on a single SQLite writer lock anyway.
- A panic in one provider task is captured as a `stats.errors` entry,
  not propagated. This matches how the GitHub fetch behaved before.

## Consequences

### Positive

- Adding a third PR provider (GitLab, Bitbucket Server, Gitea) becomes a
  pure additive change: one new module, one new config block, one extra
  `if let Some(cfg) = ...` in `build_pr_providers`. The orchestrator
  doesn't change.
- Running both GitHub and Bitbucket against the same database in a
  single `tga collect` run is now possible. Fetches happen in parallel;
  the only serialization point is the SQLite writer.
- The trait is small enough (3 methods) that it is reviewable in one
  screen and unlikely to grow into a leaky abstraction.
- All Bitbucket-specific deserialization stays inside
  `src/collect/bitbucket/`; the rest of the pipeline never sees a
  `BbPullRequest`.

### Negative

- The lossy `DECLINED|SUPERSEDED → Closed` collapse means a future
  report that wants to distinguish them either has to refetch from the
  API or wait for a schema change. Both are recoverable but neither is
  free.
- The `pull_requests` table now contains rows from multiple providers
  but has no `provider` column. Combined with the pre-existing
  `INSERT OR REPLACE` / non-UNIQUE index bug (issue #71), this means
  GitHub PR #42 and Bitbucket PR #42 against the same database double-
  insert today rather than upserting. The bug existed before this ADR
  and is tracked separately; this work makes it more reachable.
- Atlassian deprecating App Passwords means the Basic auth path may
  have a finite lifetime. The config is shaped to absorb that (Bearer
  is already supported and is the documented replacement), but we will
  need a doc and CHANGELOG update when App Passwords stop working.

### Neutral

- The Bitbucket client is the first place `wiremock` is actually used
  in this project, despite being a `[dev-dependencies]` entry for a
  while. The pattern (`api_base_url` override on the config struct →
  `MockServer::uri()` in tests) is reusable; back-porting it to the
  GitHub client is a follow-up worth doing.
- `Database` not being `Sync` continues to shape the codebase. Anything
  that wants to write to it must run on the main pipeline task. This
  isn't new, but the JoinSet wiring above makes it explicit for the
  first time.
- The PR list filter uses the four-state form because the default
  excludes merged history; if Bitbucket ever changes that default, the
  filter becomes redundant rather than wrong.

## References

- `src/collect/pr_provider.rs` — trait definition
- `src/collect/bitbucket/client.rs` — Bitbucket Cloud implementation
- `src/collect/github/client.rs` — GitHub `impl PrProvider`
- `src/collect/collector.rs::fetch_and_store_prs` — JoinSet orchestration
- `src/core/config/mod.rs::BitbucketConfig` — config schema
- `src/core/config/validator.rs::check_bitbucket_config` — validation rules
- `docs/requirements/configuration.md` — user-facing config reference
- `docs/requirements/collection.md` — pipeline-level docs
- Bitbucket Cloud REST 2.0 docs: <https://developer.atlassian.com/cloud/bitbucket/rest/intro/>
- Bitbucket Cloud pagination: <https://developer.atlassian.com/cloud/bitbucket/rest/intro/#pagination>
- Atlassian App Password deprecation context: <https://developer.atlassian.com/cloud/bitbucket/rest/intro/#authentication>
- Issue #71 — `store_pull_requests` non-UNIQUE index bug
