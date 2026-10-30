# 0073. Bare first session names; a counter only on duplicates

- **Status:** Accepted
- **Date:** 2026-10-29
- **Accepted:** 2026-10-29 (Architect ruling on [#10478](https://github.com/bobmatnyc/trusty-tools/issues/10478))
- **Scope:** crate `trusty-common` (`session_naming`), with consumers `trusty-mpm` (`session_manager::naming`, the `tm ls` picker preview) and `trusty-agents` (`tm::project_config::next_session_name`)
- **Reversibility Cost:** Medium — the rule is one function, but session names are operator-facing and appear in worktree paths, `session/<name>` branches and `ws/<name>` labels, so a reversal changes what new sessions are called again
- **Decision Drivers:** doubled names observed live (`tm-tm-mpm-bugs-01-01`, [#10478](https://github.com/bobmatnyc/trusty-tools/issues/10478); earlier `tm-tm-tcode-01`, [#3426](https://github.com/bobmatnyc/trusty-tools/issues/3426)), a `-01` on every first session that disambiguates nothing, name uniqueness that resume-by-name (#8408) relies on
- **Supersedes / Superseded by:** — (amends [ADR-0039](0039-operator-named-sessions-unique-by-construction.md))

## Context

ADR-0039 Decision point 1 kept the auto-created name as `tm-<leaf>-NN`, with a serial from `-01`. `build_session_name` in `crates/trusty-common/src/session_naming/mod.rs` built that name as `tm-` + the sanitized leaf + the lowest free serial. It never removed a `tm-` or a `-NN` the leaf already carried. A leaf that was itself a session name was therefore prefixed and numbered twice: the hint `tm-mpm-bugs-01` produced `tm-tm-mpm-bugs-01-01` (#10478). #3426 reported the same shape in 2026-07, and its fix was never applied. The serial also started at `-01`, so the first session for every project carried a counter that told it apart from nothing.

ADR-0039 also stated that a bare `tm-<leaf>` never counts as a serial, so an operator-named `tm-foo` and an auto-created `tm-foo-01` coexist. That property depended on the first auto-created name ending in `-01`.

## Decision

We will build every new managed session name by this rule:

1. **Normalize the leaf.** After sanitizing, strip a leading `tm-` and a trailing two-digit `-NN`, repeating until neither applies. A strip never empties the leaf. `tm-mpm-bugs-01` gives the leaf `mpm-bugs`; `tm-tm-x-01-01` gives `x`. The picker preview (`leaf_slug_from_hint`) applies the same strip, so it shows the name the daemon builds.
2. **The first name is bare.** When no taken name holds the leaf, the session is `tm-<leaf>`, with no counter.
3. **A counter only on a duplicate.** The taken set is unchanged: every live tmux session plus every non-terminal daemon record. If any taken name is `tm-<leaf>` or `tm-<leaf>-NN` (including a legacy `-01`, or any other `-NN` alone), the new name is `tm-<leaf>-NN` with the lowest free `NN` in `02`–`99`. Both the bare name and `-01` occupy serial 1, so a duplicate never receives `-01`. Gaps left by decommissioned sessions are still reused. All of `02`–`99` taken is the existing `SerialExhausted` error.
4. **No rename.** Existing sessions keep their names. The rule applies to sessions created after it ships; a separate, explicit rename sweep is out of scope.

The rule lives in `build_session_name`/`allocate_serial`, so trusty-mpm's create, picker and dispatch paths and trusty-agents' `next_session_name` share it. As built, a `name_hint` goes through the same allocator, so an operator-typed hint follows this rule too.

## Consequences

### Positive

- No new name starts with `tm-tm-` or ends in a doubled `-NN-NN`.
- `tmux ls` shows `tm-trusty-tools` for a project's only session; counters appear only when there is something to tell apart.
- Uniqueness across live tmux and non-terminal records holds as before: the taken set, the pre-create re-dedupe (`dedupe_session_name`, which turns a colliding bare name into `-02`) and the create retry loop are unchanged.
- Prefix-only consumers (`is_managed_session_name`, `strip_managed_prefix`, orphan GC, reconcile, adopt, `ws/<name>` labels, `session/<name>` branches) need no change; a bare `tm-x` is a managed name.

### Negative / Trade-offs

- **ADR-0039's coexistence claim no longer holds.** A bare `tm-<leaf>` now occupies serial 1, so an operator-named `tm-foo` makes the next auto-created session for leaf `foo` `tm-foo-02`, not `tm-foo-01`.
- A leaf whose real name ends in a two-digit segment (`rust-20`) loses it. Longer numeric segments (`rust-2021`) and mixed ones (`repo-v2`) are kept.
- During the transition both shapes are live: older `tm-x-01` sessions beside new bare `tm-x` sessions. Because any sibling blocks the bare name, a new session beside a live `tm-x-01` is `tm-x-02`, so it never reads as a rename of the first.

### Neutral / Follow-up work

- Renaming existing sessions to the new shape is a separate decision and PR (rename sweep with a dry run).
- ADR-0039 Decision point 2 (attach-if-live for operator-typed names) is not changed by this ADR and remains to be implemented.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-10-29:

- **ADR-0039 (Operator-named sessions are unique by construction; the per-project serial stays scoped to auto-creation):** **Amends.** Decision point 1's auto-created shape changes from `tm-<leaf>-NN` to bare `tm-<leaf>` with `-02`..`-99` on a duplicate. The positive consequence that `tm-<leaf>` and `tm-<leaf>-01` coexist without counting as the same serial is withdrawn. Points 2–6 (attach-if-live, name as a mutable key, immediate release on rename, future-only labels, the name→id index) stand.
- **ADR-0030 (A session owns many workstreams and lives in the tm checkout):** **Consistent.** Workstream identity is `workstream_id`; this ADR changes only the session's name string.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):** **Consistent.** Worktree directories named after a session take whatever name this rule produces; topology is unchanged.
- **Issue #3426 (doubled `tm-tm-` prefix):** the defect this ADR's normalization fixes.
- **Issue #3692 (auto-suffix, never reject):** **Consistent.** The create path's final safety net still suffixes a collision; a bare name collides to `-02`.
- **Issue #5301 (idempotent `tm-` prefix normalizer):** **Extends.** The normalization in Decision point 1 is the idempotent normalizer that issue asked for, applied to the counter as well.
