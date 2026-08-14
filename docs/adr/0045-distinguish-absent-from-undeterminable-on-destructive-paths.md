# 0045. Distinguish absent from undeterminable before a destructive filesystem operation

- **Status:** Proposed
- **Date:** 2026-08-11
- **Scope:** Workspace-wide; first enforced in `trusty-common`, `trusty-memory`,
  `trusty-mpm`, and `trusty-agents`
- **Reversibility Cost:** Low — the rule is a review standard over call sites,
  not a structural commitment; reversing it costs nothing but the fixes already
  landed
- **Decision Drivers:** six independent rediscoveries of the same defect in one
  crate family; `Path::exists()` coercing every stat error to `false`; a
  destructive branch taken on an uncertain probe; the absence of any mechanical
  gate that can detect the full family
- **Supersedes / Superseded by:** —

## Context

`Path::exists()` is `fs::metadata(self).is_ok()` (toolchain 1.94.1,
`library/std/src/path.rs:3429`). Every error becomes `false`, including
`EACCES` and `EPERM`. Its own documentation recommends `try_exists()` when
errors matter. `is_dir()` and `is_file()` share the property. At a call site the
expression reads as an ordinary presence check, which is why it survives review.

Six instances landed in one crate family, each found only by inspecting the site
adjacent to the previous one: `merge_palaces` (#5488), `purge_palaces` and
`rebuild_palaces` (#5526), the outer guard of `PalaceStore::list_palaces`
(#5532, #5541), four sites in that function's per-entry loop (#5543, #5545), and
`load_palace`'s own guard (#5549, still open at
[`palace_store.rs:152`](https://github.com/bobmatnyc/trusty-tools/blob/aadbbd5bb87498011238d5585786455d292dcff7/crates/trusty-common/src/memory_core/store/palace_store.rs#L152)).
The remediated walk at
[`palace_store.rs:197`](https://github.com/bobmatnyc/trusty-tools/blob/aadbbd5bb87498011238d5585786455d292dcff7/crates/trusty-common/src/memory_core/store/palace_store.rs#L197)
shows the shape the fixes converged on: `try_exists` plus an explicit
`ErrorKind::NotFound` arm, with every other error propagated and naming the
offending path.

A workspace sweep then found the variant that wears the fix's clothing:
`try_exists(...).unwrap_or(false)`. `try_exists` separates absent from
undeterminable and `.unwrap_or` throws that separation away, reconstructing
`Path::exists()` in code that reads as already remediated. 15 sites — 5 HIGH,
5 MEDIUM, 5 correct as written. The HIGH pair (#5551, #5552) is in the workflow
reconciliation helpers, at
[`helpers.rs:116`](https://github.com/bobmatnyc/trusty-tools/blob/aadbbd5bb87498011238d5585786455d292dcff7/crates/trusty-agents/src/workflow/engine/helpers.rs#L116)
and
[`helpers.rs:210`](https://github.com/bobmatnyc/trusty-tools/blob/aadbbd5bb87498011238d5585786455d292dcff7/crates/trusty-agents/src/workflow/engine/helpers.rs#L210):
an undeterminable destination reads as absent, so the code renames over real
output and deletes the source while logging a successful relocation.

### What actually triggers the coercion

The intuitive model — someone unplugged the drive — is wrong, and three measured
facts replace it.

| Scenario | What `metadata()` returns | Reaches the coercion? |
|---|---|---|
| Unmounted or unplugged volume | `Ok(false)` from `try_exists`, no `Err` | No |
| macOS TCC denial | `Ok` from `stat`; denial arrives at enumeration as `EPERM` (1) | No, not at the probe |
| POSIX denial, unsearchable parent | `Err` at `stat` with `EACCES` (13) | Yes |
| `ENOTDIR`, `ELOOP` | `Err` at `stat` | Yes |
| Transient `EIO` / `ETIMEDOUT` / `ESTALE` on a network mount | `Err` at `stat` | Yes |

The transient class is the sharpest. A destructive branch needs the error to
clear between the probe and the mutation, which is exactly what a transient
network-filesystem error does: the probe says absent, the `rename` a moment
later succeeds against the file that was there all along.

## Decision

On any path whose result feeds a **destructive, enumerating, or
operator-reporting** operation, a fallible probe must distinguish *genuinely
absent* from *cannot determine*. `NotFound` takes the benign path; every other
error propagates.

1. Use `try_exists()`, or match on `io::ErrorKind` from `metadata()`. Not
   `exists()`, `is_dir()`, or `is_file()` on such paths.
2. `try_exists(...).unwrap_or(...)` is **prohibited** on such paths. It is
   self-contradictory: nobody reaches for `try_exists` unless the error matters.
3. 🔴 `NotFound` keeps its benign behaviour. A data root that does not exist yet
   is normal first-run. Making absence an error has a bigger blast radius than
   the bug it fixes, so every fix verifies this boundary per site.
4. Fail-open is legitimate where it is the SAFE direction, and must carry
   `// intentional fail-open: <reason>`. The worked example is the provenance
   prune at
   [`mcp_provenance.rs:304`](https://github.com/bobmatnyc/trusty-tools/blob/aadbbd5bb87498011238d5585786455d292dcff7/crates/trusty-mpm/src/core/mcp_provenance.rs#L304),
   whose `retain` keeps a claim on uncertainty: dropping one risks
   misattributing a later file written at the same path.
5. Closure condition for any fix in this family is an **error-arm regression
   test that fails against the pre-fix commit** — this repo's Fail-Open Check.
   `list_palaces_propagates_an_unstattable_entry` and its three siblings in
   `palace_store.rs` are the reference shape.

### Alternatives considered

| Alternative | Verdict | Why |
|---|---|---|
| clippy `disallowed-methods` on `exists` / `is_dir` / `is_file` | Rejected | 1,710 call sites workspace-wide, overwhelmingly legitimate. Unlandable as a deny, and a 1,700-entry `#[allow]` campaign is worse than the defect. Config is per-crate-directory, so rollout would be per-crate anyway. |
| `scripts/check_*.sh` grep for `try_exists(...).unwrap_or(` | Marginal | Flags all 15 sites, 10 correctly, 5 needing an escape hatch. All eight `helpers.rs` sites are the byte-identical token; overwrite-on-uncertainty and skip-on-uncertainty differ only in which side of an `if`/`!` the call sits and what the guarded block does. Grep cannot see that. Its value is forcing the author to state the direction, not detection. |
| dylint custom lint | Deferred on cost, not rejected on merit | The precise rule: flag `try_exists(...).unwrap_or(false)` only where the guarded branch reaches `rename` / `copy` / `remove_file` / `remove_dir_all` in the same function. That flags exactly the HIGH sites. |
| Mechanical detection of the whole family | Not possible | The defect is the coercion PLUS a destructive downstream consumer. That second half is interprocedural dataflow, which clippy is structurally unable to do. The question is settled, not open. |

## Consequences

- The class becomes reviewable by citation. A reviewer points at this ADR
  instead of rediscovering the reasoning from the call site, which is what
  happened six times.
- No gate enforces it. Enforcement is review discipline, and the grep and dylint
  options above are the only candidates that were examined and priced.
- Every fix must verify the `NotFound` boundary at its own site. That is real
  per-site work, and skipping it converts a silent-skip bug into a first-run
  failure.
- The `try_exists(...).unwrap_or(...)` prohibition invalidates a pattern that
  currently reads as remediated in 15 places; 5 of those are correct as written
  and will need the `// intentional fail-open:` comment rather than a code
  change.
- Error-arm tests cost more than presence tests: they need an unstattable path,
  which means fixture permissions rather than a missing file.

## Related Decisions

Vetted against the ADR corpus on 2026-08-11:

- **ADR-0023 (Worktree authority — existence vs ownership):** Extends. Its
  decision 5 requires the destructive worktree-reclamation path to fail closed
  on a missing, empty, or stale index. This ADR applies the same rule one layer
  down, to the filesystem probe that feeds a destructive operation, and adds the
  `NotFound`-stays-benign boundary that ADR-0023 does not address.
- **ADR-0034 (Durable webhook relay):** Consistent. Console spools durably
  before acknowledging rather than treating an unconfirmed write as done; no
  overlap in scope.
- **ADR-0035 (Console health probe aggregates over UDS):** Consistent. An
  unreachable service is reported as unreachable rather than coerced to a
  benign state, which is the operator-reporting half of this rule expressed for
  one subsystem.
- **ADR-0043 (Traceable `$CARGO_HOME/bin` provenance, Proposed):** Consistent.
  Both classify an unknown explicitly instead of folding it into a benign
  bucket; no shared mechanism.

No prior ADR governs filesystem-probe error handling, and no Accepted decision
contradicts this one.
