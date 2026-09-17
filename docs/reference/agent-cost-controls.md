# Agent Cost Controls — Embedded Assets, Shared Target Dir, Pre-Claim Checks

> Three repo-specific rules from one measured incident on 2026-09-17
> ([#8251](https://github.com/bobmatnyc/trusty-tools/issues/8251)). An agent
> editing five markdown files ran `cargo test -p trusty-mpm` in a fresh
> worktree with an empty `target/`. That was a cold build of the crate and its
> whole dependency graph: 2h22m wall clock and ~278,000 tokens across two runs.
> [`CLAUDE.md`](../../CLAUDE.md) keeps the headline of each rule and points
> here for the mechanics.
>
> The general, project-agnostic half of the same fix lives in the bundled
> assets, not here: the transcript cost model and the evidence-vs-progress rule
> in `BASE-AGENT.md`, the Rust build levers in the `rust-build-performance`
> skill. This page states only what is specific to this repo.

## 1. Markdown under `crates/*/src/assets/**` is not a rung-1 change

These files look like docs and are not. They are compiled into the binary with
`include_str!`, and bundle tests assert on their text. Verified embed sites:

| Asset tree | Embedded at |
|---|---|
| `crates/trusty-agents-common/src/assets/agents/*.md` | `crates/trusty-agents-common/src/agent_assets.rs` |
| `crates/trusty-mpm/src/assets/skills/*.md` | `crates/trusty-mpm/src/core/bundle_tm_skills.rs`, `bundle_skills_*.rs` |

Two consequences the rung-1 row does not cover. Editing one recompiles the
embedding crate and every crate that depends on it — `trusty-agents-common` is
a dependency of both `trusty-mpm` and `trusty-code`, so an agent-asset edit is
a cross-crate recompile. And a bundle test can go red on a text edit alone,
because it asserts on section headings, section ORDER, and frontmatter keys.

**Classification: rung 3, with a named narrow gate — not the crate suite.**
The blast radius of a text edit is the tests that read that text. Run those,
plus the doc gates rung 1 already owes.

```bash
cargo test -p trusty-mpm --lib core::bundle::tests
cargo test -p trusty-mpm --lib core::claude_md_sections::tests
```

🔴 **The filter is the MODULE path, not the source basename.** Both files are
pulled in with `#[path = "…_tests.rs"] mod tests;`, so their basenames appear in
no test name. `core::bundle_tests` matches nothing, reports `0 passed; …
filtered out`, and exits 0 — a green run that proved nothing. That is the
#7866 class, described at
[test-ladder-baseline.md](test-ladder-baseline.md#a---path-included-module-filters-by-module-path-not-source-basename-7866).
Read the passed count, never just the exit code.

The tests those two module filters carry, confirmed present:

| Asset edited | Test that reads it |
|---|---|
| `assets/agents/BASE-AGENT.md` | `base_agent_guidance_sections_survive_composition` — composes `version-control` and `local-ops` from the real assets dir and asserts sections and their order |
| `assets/skills/rust-build-performance.md` | `rust_build_performance_skill_is_in_bundle` — `ALL` registration plus frontmatter |
| `assets/skills/tm-delegation-patterns.md` | `the_relocated_routing_detail_is_carried_by_the_delegation_skill`, `pm_re_engagement_checks_worktree_survival_before_resuming_8004` |

`--lib` here is a **scope claim about blast radius**, not a way to make a red
gate green — the distinction `CLAUDE.md`'s "scope down, never scope away" rule
draws. A failure inside the filter means widening to `cargo test -p <crate>
--no-fail-fast`, never narrowing further.

Two more gates an asset edit can owe, both cheap and neither a Cargo build:

- `scripts/check_agent_assets.sh` — runs when an agent asset changes. Four
  `trusty-code` copies are pinned deviations by sha256 in
  `scripts/agent-asset-pins.tsv`; editing their shared source fails the gate
  until the fork is reconciled. `BASE-AGENT.md` is not one of the four.
- `scripts/check_context_budget.sh` — `CLAUDE.md`, the instruction sections and
  the output style are a per-turn cost, gated against a committed baseline.
  Growth that is genuinely warranted is one `--update` away; growth that is not
  belongs in a skill or here.

## 2. The shared `CARGO_TARGET_DIR` comes from the repo-local `.envrc`

A repo-local `.envrc` exports `CARGO_TARGET_DIR` for this checkout and every
worktree under it. **Agents must not override it, and must not point a gate at
a worktree-local `target/`.** 65 worktrees each holding an independent
`target/` measured ~850 GB on 2026-09-17 and defeated cache reuse entirely.

`.envrc` is the operator's local mechanism. The declarative one that tm itself
ships is the `build:` section of `~/.trusty-tools/trusty-mpm/config.yaml`,
reported by the `rust_build_env` `tm doctor` row
([#6868](https://github.com/bobmatnyc/trusty-tools/issues/6868),
`crates/trusty-mpm/src/core/build_env.rs`). Use that mechanism rather than
inventing a per-brief environment variable.

What the shared directory does and does not buy, measured in this worktree on
2026-09-17 running the narrow gate above: registry dependencies were reused
from the shared directory and did not rebuild, while the four workspace path
crates did rebuild. Cargo fingerprints a path crate by its absolute source
path, so a different worktree path is a different fingerprint. The shared
directory removes the dependency-graph cost, not the workspace-crate cost.

## 3. A pre-claim check must read LOCAL worktrees, not only the remote

`git ls-remote` and `gh pr list` see pushed branches and open PRs. They do not
see a local worktree holding unpushed commits. On 2026-09-17 that gap produced
a full duplicate implementation of #8233.

Before claiming an issue or dispatching work on it, add the local half:

```bash
git worktree list
git branch --list
git log --oneline origin/main..<branch>
```

`git worktree list` names every tree and its branch; `origin/main..<branch>`
shows commits that exist nowhere but this machine. A branch with commits and no
remote counterpart is an in-flight claim, and the tree holding it is the tree
that must finish it — see
[worktree-discipline.md](worktree-discipline.md) for reading another tree's
state without cross-tree git, and `tm session adopt-worktree` for taking over a
dead owner's tree.

The structural fix — making the check mechanical rather than a remembered step
— is tracked in
[#8250](https://github.com/bobmatnyc/trusty-tools/issues/8250).
