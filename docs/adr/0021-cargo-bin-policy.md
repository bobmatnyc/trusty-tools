# 0021. ~/.cargo/bin contains registry installs only

- **Status:** Proposed
- **Date:** 2026-07-26
- **Amended:** 2026-08-06 — see [Amendment 1](#amendment-1--2026-08-06-the-prebuilt-downloader-is-a-third-provenance-class-4964)
- **Scope:** Workspace-wide (all trusty-* crates with binary artifacts)
- **Reversibility Cost:** Low — the policy is a constraint on what goes into `~/.cargo/bin`, not a refactor of existing code; reverting it changes permissions but requires no migrations
- **Decision Drivers:** Visibility of installed binaries, traceability of their source code state, inability to detect bit-rot and version skew after source worktrees are deleted, mechanical checkability via issue #4033 (tm doctor)
- **Supersedes / Superseded by:** —

## Context

An inventory taken on 2026-07-26 found **TEN trusty-* binaries installed in `~/.cargo/bin` via `cargo install --path`** from git worktrees, every one of which is now missing — deleted both as directories AND as git worktree records, almost certainly as collateral from a 2026-07-17 cleanup that reclaimed ~1.1 TiB by removing 63 stale worktrees.

**Consequences observed:**

- `trusty-search` was running `0.39.0` while `0.39.1` shipped that same day carrying two crash-safety fixes (#3690, #3691). A pending performance measurement for issue #3748 would have been run against the wrong binary, producing a confident wrong answer.
- `tctl` was seven patch versions stale at `0.4.3` against the published `0.4.10`.
- `trusty-console` was a full minor version behind the shipped release.
- `trusty-channels` was installed **twice** from two different worktrees providing different binaries at the same version number; because both worktrees are gone it is **permanently unknowable** whether those binaries were built from the same code.
- Dirty/unpushed state for all ten is permanently undetectable — not merely unchecked at install time, but unrecoverable.

**Why this root cause occurred:** `cargo install --path <worktree>` creates a persistent binary in `~/.cargo/bin` sourced from an impermanent worktree. When the worktree is deleted, the binary outlives its source — invisible to any inventory or rebuild process, able to diverge silently from the published versions, and carrying no metadata about its source commit or branch state. The standard Unix convention is that installed binaries come from a trusted registry (crates.io, in this case) or from a persistent build directory; a temporary worktree is neither.

**Why the rule costs nothing:** Testing and development runs a binary directly from `target/{debug,release}/`, which is unrestricted — developers can build from any branch and run the result immediately. The escape valve is: "If I want to use a one-line fix right now, I build it once (`cargo build --release -p <crate>`) and run `./target/release/<binary>` for this session." This is the standard developer workflow and is not altered by the policy.

**Why only trusty-* crates matter for this policy:** The rule applies to crates this workspace maintains. External crates (vendored dependencies, third-party tools) can be installed from crates.io normally; they are not subject to this constraint.

**Mechanical checkability:** Issue #4033 proposes a `tm doctor` gate that checks `cargo install --list` for any entry showing a path in parentheses (indicating a path-install source). That check is **unambiguous** — no false positives, no design judgment needed. This ADR anchors the policy so the gate can be adopted and enforced.

## Decision

We will adopt the **registry-install policy**:

> `~/.cargo/bin` contains **only registry installs** (`cargo install <crate>`), never path installs (`cargo install --path`).
>
> Testing and development runs binaries directly from `target/{debug,release}/`, unrestricted.
>
> No trusty-* crate ever uses `cargo install --path`, whether in a script, a CI job, a session setup, or an interactive shell. The escape valve for running an unreleased fix immediately is: build it (`cargo build --release -p <crate>`) and run the binary directly from the target tree for that session only.
>
> Enforcement: `tm doctor` checks `cargo install --list` for path-install entries and reports them as violations (issue #4033).

This rule is **mechanically checkable** and **enables future audits** of `~/.cargo/bin` to detect bit-rot, version skew, and orphaned binaries. Any violation is unambiguous.

## Consequences

**Easier / positive:**

- Every binary in `~/.cargo/bin` now has an auditable source: `cargo install --list` shows the version, and crates.io holds that exact version.
- Bit-rot detection becomes possible: a comparison of `cargo install --list` against published versions (or a local `cargo tree`) will catch stale installs.
- Orphaned binaries (from deleted worktrees) cannot happen — every install is tied to a crate.io version that is immutable and persistent.
- No more silent version skew: if a fix ships, Bob either upgrades the installed version (via `cargo install <crate> --upgrade`) or runs from `target/` for the session.
- Reproducibility: the binary installed on two machines at the same crate version is guaranteed to be identical (modulo platform).

**Harder / negative / trade-offs:**

- **Every fix Bob wants to use immediately requires a release cycle.** That is the documented, accepted cost. A one-line fix he wants right now cannot go into `~/.cargo/bin` until it ships. **This is mostly a benefit** — it forces an honest release cadence and ensures users get the same code Bob dogfoods — but it means patience is required. The stated escape valve (run from `target/` for the session) is unrestricted and is the standard developer workflow; it is not a degradation.
- **Three crates block this policy from taking effect immediately** (see Blocking Prerequisites, below): `trusty-agents`, `trusty-channels`, and `trusty-gworkspace` have no (or only yanked) versions published to crates.io, so Bob is already in violation using his daily tooling. These must be published before the policy can be adopted.
- Adoption via `tm doctor` (issue #4033) and this ADR is blocked by resolving those prerequisites.

**Known gaps / follow-up work:**

- The policy applies only to binaries Bob installs. Binaries installed by **other users** (team members, CI bots) are outside this scope but should follow the same convention — worth noting in documentation if this is formalized (ADR scope is workspace-wide, not user-wide).
- This policy does not constrain where Bob builds binaries for *distribution* (e.g., release binaries, CI artifacts); it constrains only `~/.cargo/bin` on his development machine.

## Blocking Prerequisites

**This policy cannot be enforced until the following crates are published to crates.io.**

Three trusty-* crates ship binaries but have no published versions, or only yanked versions. Declaring this policy before they are published would put Bob in immediate violation:

1. **trusty-agents** (binaries: `tagent`, `ompm`) — never published. Must publish to crates.io (first publish — requires version decision, metadata/licence check, and `cargo publish --dry-run`).

2. **trusty-channels** (binaries: `slack-mcp`, `telegram-mcp`) — never published. Must publish to crates.io (first publish — same requirements).

3. **trusty-gworkspace** (binary: `gworkspace-mcp`) — current version 0.2.2 is marked `publish = false`. Version 0.1.0 is yanked. Either un-yank 0.1.0 or publish 0.2.2 to crates.io.

See the related issues filed under this ADR for detailed publish checklists (publish only from merged main, `--dry-run` must pass, cross-crate ordering matters if dependencies are involved).

**Adoption gate:** Once 1–3 above are resolved, issue #4033 (`tm doctor` gate) can be landed, and this ADR's status can move to Accepted.

## Amendment 1 — 2026-08-06: the prebuilt downloader is a third provenance class (#4964)

**Amended, not superseded.** The decision above is unchanged and unreversed: no
trusty-* crate uses `cargo install --path`, and every *cargo* install into
`$CARGO_HOME/bin` is a registry install. What this amendment adds is a class the
original text never enumerated, because at the time it could not occur.

### What changed underneath the ADR

The original text reasons about `~/.cargo/bin` as a directory only cargo writes
to, so every file in it has a ledger record and the only open question is which
*kind* of record — registry, path, or git. Epic
[#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964) consolidates every
install destination this repo controls onto `$CARGO_HOME/bin` (falling back to
`~/.cargo/bin`), which makes the prebuilt downloader a legitimate writer into
that directory. The downloader keeps no cargo metadata, so it produces files with
**no ledger presence at all**, or with a ledger record that describes the version
it just replaced.

### The amended policy

`$CARGO_HOME/bin` now holds files from exactly three sanctioned writers:

| Class | Written by | Ledger presence | `tm doctor` `binary_provenance` |
|---|---|---|---|
| **Registry install** | `cargo install <crate>` | accurate record, `registry+` source | `Ok` when the versions agree |
| **Prebuilt placement** | `tctl install` / `tctl upgrade` / `install.sh` — the downloader | absent, or stale (describes the replaced version) | `Unknown` |
| **Path install** | `cargo install --path` | accurate record, `path+file://` source | `Warn` — **still a violation of this ADR** |

The path-install row is unchanged by this amendment and remains prohibited.
`Warn` is its CORRECT and terminal verdict; it is not a defect to be chased to
`Ok`.

The prebuilt-placement row is **permitted**. It is not a path install, it is not
invisible to update detection (`tctl upgrade` and the daemons' own update check
query crates.io directly, not cargo's ledger), and it is exactly what the
toolchain-free install path is for.

### Why `Unknown` rather than `Ok` or `Fail`

`Ok` would be a false claim: the check reads cargo's ledger, and for a
downloader-placed file the ledger either says nothing or says something stale.
The module's founding rule (#4033) is that a probe which learned nothing reports
`Unknown`.

`Fail` would be a false alarm, and specifically a *new* one manufactured by this
epic. Before #4964 Phase 3 a downloader-placed binary sat in `~/.local/bin`,
failed `provenance_report`'s same-file check, and reported `Unknown`. After the
flip the identical file sits in `$CARGO_HOME/bin`, the same-file check now
succeeds, and the stale ledger version disagrees with the running one. Nothing
about the binary changed — only its directory. The verdict must not change
either.

### The integrity check that survives

Reporting every ledger/binary version disagreement as `Unknown` would have
retired a real check, so the disagreement is split by direction:

- **running binary NEWER than the ledger record** → `Unknown`. Cargo rewrites its
  ledger whenever it writes, so a newer file at that path came from a writer that
  keeps no cargo metadata. Sanctioned under this amendment.
- **running binary OLDER than the ledger record** → `Fail`. An older binary is
  sitting on top of a newer install: the upgrade the ledger says landed is not
  what executes. This is the #4033 incident verbatim and keeps its severity.
- **the two versions cannot be ordered as semver** → `Fail`. An unverifiable
  mismatch is still a mismatch.

Implementation and per-branch tests: `crates/trusty-mpm/src/core/binary_provenance.rs`
(`version_skew_verdict`, `classify_version_skew`).

### What this amendment does NOT settle

- **The `tm doctor` gate in the Decision section is still the path-install gate.**
  It checks `cargo install --list` for path-install entries. This amendment adds
  no new mechanical gate; the prebuilt class is *observed* by
  `binary_provenance`, not enforced.
- **The Blocking Prerequisites are unchanged**, so the status stays **Proposed**.
  Three crates (`trusty-agents`, `trusty-channels`, `trusty-gworkspace`) are
  still unpublished or yanked.
- **Homebrew remains out of scope.** Per the repo owner's ruling on #4964
  (2026-08-06), `$(brew --prefix)/bin` is a fourth destination this repo cannot
  redirect, and PATH collisions with it are accepted residual risk. Any statement
  that `$CARGO_HOME/bin` is the single install location means *excluding
  Homebrew-installed copies*.

## Related Decisions

Vetted against prior ADRs on 2026-07-26:

- **ADR-0002 (Single-install convention):** **Consistent / Extends.** ADR-0002 established that "all major crates install to the same location via single `cargo install` command." This ADR refines that into a directive about **how** to install (registry only, not path), ensuring the installs remain traceable and auditable. No conflict; ADR-0002 is still the install UX, and this ADR governs the source of those installs.

- **ADR-0001 (Design/research/ADR docs live in top-level `docs/`):** **Consistent.** This ADR itself lives in `docs/adr/`, following ADR-0001. No interaction with other crates or subsystems.

- **ADR-0003 (MSRV and edition policy):** **No interaction.** MSRV affects which Rust features can be used; install source does not.

- **ADR-0008 (Project-identity convention):** **No interaction.** Project identity is orthogonal to install source.

- **ADR-0011 (tctl owns service lifecycle; trusty-console owns HTTP surface) / ADR-0018 (Loopback-only doctrine):** **No interaction.** Those ADRs govern service startup and HTTP topology; install source is an orthogonal concern.

No conflicts with any Accepted ADR. Summary: consistent with, and a direct refinement of, ADR-0002; no silent contradictions.
