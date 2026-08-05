# 0029. MSRV 1.94 and per-crate Rust edition policy

- **Status:** Accepted
- **Date:** 2026-08-05
- **Scope:** Workspace-wide
- **Supersedes / Superseded by:** Supersedes [0003](./0003-msrv-and-edition-policy.md)

## Context

[ADR-0003](./0003-msrv-and-edition-policy.md) set the workspace MSRV to **1.88**
— the Rust release that stabilized let-chains, the one language feature the
`trusty-mpm` / `open-mpm` families actually needed — and recorded that raising
the MSRV later would itself be architecturally significant and require a
superseding ADR. That ADR was never written, and the floor has since moved
twice. This ADR records both moves and re-states the policy at its current
value.

**Both bumps were forced by AWS SDK MSRV drift, not by our own code.** The
workspace has never needed a language feature newer than 1.88's let-chains. The
floor is set entirely by the AWS crates that back `trusty-common`'s optional
`bedrock` feature (and its consumers, `trusty-agents` and `tga`).

### Bump 1 — 1.88 → 1.91 (2026-05-31, unrecorded)

A `Cargo.lock` refresh pulled `aws-smithy-*` / `aws-runtime` indirect
dependencies that declare `rust-version = "1.91.1"`. CI had already been running
its MSRV job on `dtolnay/rust-toolchain@1.91` with a TODO comment; commit
`7efc0a3b` ("chore: correct workspace MSRV to 1.91", #525) corrected
`[workspace.package].rust-version` and the per-crate pins to match reality. No
ADR was written at the time, so ADR-0003 has been describing a floor the
workspace has not enforced for over two months.

### Bump 2 — 1.91 → 1.94 (this ADR)

A user's `cargo install trusty-code` failed on rustc **1.91.1**. The root cause
is that `cargo install` **without `--locked` ignores our committed
`Cargo.lock`** and re-resolves the dependency graph inside the caret ranges
declared in the root `Cargo.toml`:

```toml
aws-config             = { version = "1.8", features = ["behavior-version-latest"] }
aws-sdk-bedrockruntime = { version = "1.131" }
```

On **2026-07-08** AWS published `aws-config 1.9.0` and
`aws-sdk-bedrockruntime 1.136.0`, both declaring `rust-version = "1.94.1"`.
Both are inside our caret ranges, so a fresh `cargo install` resolves to them
and then refuses to build on any toolchain below 1.94.1 — even though
`cargo install --locked` (which honours the lockfile's 1.8.17 / 1.131.0) still
works fine. The failure is therefore not reproducible in CI, which builds
`--locked`, but it is exactly what a first-time user hits.

Three options were considered:

1. **Pin the AWS ranges** to the last 1.91-compatible versions (`=1.8.17`,
   `=1.131.0`) — keeps the 1.91 floor but freezes us out of AWS security and
   model updates, and needs re-pinning on every AWS release.
2. **Gate Bedrock behind a non-default feature** everywhere — keeps the floor
   low for non-AWS crates but splits the build matrix and does not help the
   crates that genuinely want Bedrock.
3. **Raise the workspace MSRV to 1.94.**

Rust **1.94.1 has been stable since 2026-03-26**, roughly four months at the
time of writing, so the toolchain is widely available via `rustup update`. A
prior research pass verified that `cargo check -p trusty-code --locked` builds
clean on rustc 1.94.1 with no source changes.

## Decision

We will raise the workspace **MSRV to `1.94`** (shared via
`[workspace.package].rust-version`, plus the four crates that carry a hardcoded
pin instead of inheriting: `tga`, `trusty-bm25-daemon`, `trusty-mpm-gui`,
`trusty-code-gui`).

We **carry ADR-0003's per-crate edition policy forward in substance, restated to
match what the workspace actually does today.** ADR-0003 framed 2021 as the
default and 2024 as the opt-in for let-chain users; the workspace has since
inverted that. The rule is now:

- `[workspace.package].edition = "2024"` is the **default**; a crate gets it by
  writing `edition.workspace = true` or declaring `edition = "2024"`.
- A crate opts **down** to `edition = "2021"` by declaring it explicitly. Ten
  crates currently do: `trusty-analyze`, `trusty-bm25-daemon`,
  `trusty-channels`, `trusty-embedderd`, `tga`, `trusty-installer`,
  `trusty-memory`, `trusty-progress`, `trusty-search`, `trusty-sld-lint`.

The contributor-facing obligation is unchanged from ADR-0003: check a crate's
`Cargo.toml` before assuming its edition, and never copy let-chain syntax into
an edition-2021 crate.

Every toolchain pin that enforces the floor moves with it: the `msrv` job in
`.github/workflows/ci.yml` (`dtolnay/rust-toolchain@1.94`, cache key
`msrv-1-94`), `capabilities-drift.yml`, `sld-lint.yml`, the in-container
`rustup --default-toolchain 1.94` in `al2023-build.yml` and `release.yml`, and
every `al2023-1.94-*` cache key. **Cache keys are part of the pin** — a stale
key silently restores a 1.91-built `target/` under a 1.94 toolchain and hides
the very drift the job exists to catch.

We deliberately **do not** pin the AWS caret ranges as part of this change.

## Consequences

- **Positive:** the declared MSRV once again matches what `cargo install`
  actually resolves, so the reported failure mode disappears for users on a
  current toolchain. The AWS ranges stay open, so Bedrock model and security
  updates keep flowing without per-release manifest churn. No source changes
  were needed — 1.94.1 compiles the workspace as-is.

- **Negative (the accepted tradeoff):** the floor is workspace-wide, so it
  reaches crates that never touch AWS. A user on rustc 1.91–1.93 must run
  `rustup update` before installing any `trusty-*` crate whose dependency graph
  reaches the AWS SDK — which, through `trusty-common`'s optional `bedrock`
  feature and its consumers, is most of them. We accept this: a uniform floor is
  worth more than a split matrix in which each crate advertises a different
  minimum that `cargo install`'s re-resolution can invalidate anyway.

  **What the manifests actually declare, counted at this commit.** Of the 27
  crate directories under `crates/` (the `members = ["crates/*"]` glob), 21 are
  publishable and 6 carry `publish = false` (`tc-services`,
  `trusty-agents-local`, `trusty-code-gui`, `trusty-cto-db`, `trusty-mpm-gui`,
  `trusty-publish-guard`). But only **10 of those 21 declare an MSRV at all**:
  8 inherit it (`rust-version.workspace = true` — `trusty-agents-common`,
  `trusty-code`, `trusty-console`, `trusty-installer`, `trusty-mpm`,
  `trusty-progress`, `trusty-review`, `trusty-tui`) and 2 pin `1.94` literally
  (`trusty-bm25-daemon`, `tga`). The other 11 — `trusty-agents`,
  `trusty-analyze`, `trusty-channels`, `trusty-common`, `trusty-embedderd`,
  `trusty-embedderd-py`, `trusty-gworkspace`, `trusty-kb`, `trusty-memory`,
  `trusty-search`, `trusty-sld-lint` — have no `rust-version` key, and
  `[workspace.package]` does not reach a member that has not opted in. They
  therefore advertise **no** floor: on an old toolchain cargo does not refuse
  them up front with a legible "requires rustc 1.94" message, it starts the
  build and fails later wherever the AWS crates land, or succeeds if the graph
  never reaches them. Giving those 11 `rust-version.workspace = true` is the
  obvious fix and is deliberately **not** done here — it changes what each crate
  advertises to crates.io, which is a publishing decision that deserves its own
  review rather than a side effect of moving the floor. Tracked as follow-up.

- **Negative (known residual risk, consciously accepted):** the AWS entries at
  root `Cargo.toml:264-265` remain **unpinned caret ranges**
  (`aws-config = "1.8"`, `aws-sdk-bedrockruntime = "1.131"`). The next time AWS
  raises its own MSRV past 1.94, a non-`--locked` `cargo install` will
  re-resolve into it and reproduce this exact failure. CI will not catch it,
  because CI builds `--locked`. This ADR is the record that the owner chose
  recurrence-with-open-ranges over the maintenance cost of pinning; the
  mitigation, if and when it recurs, is to re-run this same decision (pin, gate,
  or raise) rather than to be surprised by it.

- **Neutral:** raising the MSRV again remains architecturally significant and
  must be recorded as a superseding ADR — the obligation ADR-0003 created and
  that this ADR is partly written to discharge. Historical `1.91` mentions in
  changelogs, past ADRs (e.g. [0014](./0014-native-mcp-support.md)), PRDs,
  research notes, and regression-test records are left untouched: they are
  records of what was true then, not assertions about what is true now.

## Related Decisions

Vetted against [`INDEX.md`](./INDEX.md) per DOC-46 §3.

- **[ADR-0003](./0003-msrv-and-edition-policy.md) (MSRV 1.88 and per-crate Rust
  edition policy):** **Supersedes.** 0003's MSRV value is replaced; its
  per-crate edition policy is carried forward in substance and restated to match
  the workspace's current 2024-by-default arrangement. 0003 is marked
  `Superseded by 0029` in its own header and in `INDEX.md`.

- **[ADR-0002](./0002-single-install-convention.md) (Single-install convention
  for main crates):** **Consistent, and the reason this ADR matters.** 0002 makes
  `cargo install` the one supported install path for every main crate, which is
  exactly why a floor that only `--locked` builds satisfy is a user-visible
  defect rather than a packaging detail. Nothing in 0002's convention changes;
  the toolchain it presumes moves.

- **[ADR-0014](./0014-native-mcp-support.md) (Ship full native MCP support):**
  **Consistent.** 0014 §Context cites "MSRV 1.91" in passing as background for
  its Rust-only toolchain argument. That line is a record of what was true when
  0014 was written, not a live assertion, and is deliberately left unchanged —
  the argument it supports (one toolchain, no polyglot sidecars) is unaffected
  by which floor that toolchain sits at.

- **ADRs 0004–0028 generally:** **No interaction.** They govern runtime
  architecture — harness topology, event bus, IPC, worktree ownership,
  credential authority, memory tiers — none of which constrains or is
  constrained by the compiler floor. This ADR changes no interface, process
  boundary, or data model.
