# 0006. Name the stack control plane `trusty-controller` (binary `tctl`)

- **Status:** Accepted
- **Date:** 2026-06-08
- **Scope:** Workspace-wide (new crate `trusty-controller`; adds `tctl` to the
  CLAUDE.md abbreviation table; consumed by the entire trusty-controller design
  set under `docs/trusty-controller/research/02-design/`)
- **Supersedes / Superseded by:** —

## Context

The trusty-controller design set (DOC-0 .. DOC-10, see
`docs/trusty-controller/research/02-design/`) specifies a single, thin
coordinator that manages install, upgrade, restart, configuration, doctor, and
health across the **whole claude-mpm stack** (claude-mpm + trusty-tools) at both
the system and project scope. The tool contains zero tool-specific logic; it
operates through a versioned per-tool contract and a stack manifest/BOM.

DOC-0 (the naming & documentation charter) had to pick the tool's real name —
the crate name, the directory under `crates/`, and the binary on PATH — before
any downstream doc could finalize filenames, manifest entries, `.mcp.json` keys,
or `<tool>`-shaped contract examples. The name is consumed by every other doc in
the set and by the root `CLAUDE.md` abbreviation table, and it will appear in
project-scoped config keys (DOC-3's project-identity convention references it).
That makes the choice both architecturally significant and costly to reverse —
renaming after the design set, crate, config keys, and published crate exist
would be expensive — so it is recorded as an ADR per the repo's ADR bar
(`docs/adr/README.md`).

Four candidates were considered:

1. **`trusty-controller`** — descriptive: the tool *is* a control plane. Slightly
   long as a binary name, but the repo already separates long crate/dir names
   from short binary aliases (e.g. `tga`, `tm`, `ts`).
2. **`trusty-ctl`** — short, reads kubectl-style. Rejected as the crate name: it
   is terse to the point of being opaque about what the tool *is*, and the
   "control" semantics it gestures at are better captured by a binary alias than
   by the crate name itself.
3. **`trusty-installer`** — rejected: too install-specific. The tool is a full
   control plane (upgrade, restart, config, doctor, health), not just an
   installer; naming it after one verb understates its scope.
4. **Reuse `trusty-tools`** — rejected: nomenclature clash with the monorepo
   name. `trusty-tools` is the workspace itself; a member crate cannot share that
   name without confusion, and the controller's remit is the *whole stack*
   (including external claude-mpm), not only the trusty-tools workspace.

Repo precedent strongly favors a descriptive crate/directory name paired with a
short binary alias. The abbreviation table in `CLAUDE.md` and the `crates/*` glob
in the root `Cargo.toml` (which auto-discovers any new member directory) support
this split cleanly.

## Decision

We will name the stack control plane **`trusty-controller`**:

- **Crate name:** `trusty-controller`
- **Directory:** `crates/trusty-controller/`
- **Binary on PATH:** `tctl`
- **Abbreviation/alias:** `tctl` (to be added to the `CLAUDE.md` abbreviation
  table as `tctl → trusty-controller`).

The `trusty-` prefix is retained even though the tool's remit spans the entire
claude-mpm stack (not only trusty-tools): the prefix marks it as part of the
trusty-* family, and the descriptive `-controller` suffix conveys that it is the
control plane. The descriptive crate name + short `tctl` binary alias follows the
established repo pattern (`tga`, `tm`, `ts`).

`trusty-controller` will be **published to crates.io** using the same manual
release workflow as the other UI-embedding crates (trusty-search, trusty-memory):
version bump → `git tag trusty-controller-v<version>` →
`SKIP_UI_BUILD=1 cargo publish -p trusty-controller` (it ships an embedded web UI,
DOC-7) → `cargo install --path crates/trusty-controller --locked` → graceful
`launchctl bootout`/`bootstrap` restart.

## Consequences

**Easier / positive:**

- Every downstream design doc (DOC-1 .. DOC-10) can now finalize filenames,
  manifest entries, contract examples, and `.mcp.json` / dispatch keys against a
  fixed name.
- The descriptive crate name self-documents the tool's role; the short `tctl`
  binary keeps day-to-day CLI use ergonomic, consistent with `tga`/`tm`/`ts`.
- Creating `crates/trusty-controller/` requires no workspace-manifest edit beyond
  the crate itself — the `crates/*` glob auto-discovers it.

**Harder / trade-offs:**

- The crate name (`trusty-controller`) differs from the binary (`tctl`), so
  newcomers must learn the mapping. This is mitigated by adding the alias to the
  `CLAUDE.md` abbreviation table (a tracked follow-up).
- Publishing to crates.io commits the project to the full manual-publish
  discipline (version bumps, per-crate tags, `SKIP_UI_BUILD=1`, local
  `cargo install`, graceful daemon restart) for this crate going forward.

**Forward-compatibility requirement (orchestrator swap):**

- `trusty-controller` is the control plane for the *entire* stack, and the
  **orchestrator is a pluggable stack member**, not hard-wired. Today the stable
  orchestrator is **claude-mpm** (Python, external repo), coordinated via a
  Python contract adapter (DOC-6). The planned in-house replacement is
  **trusty-mpm** (`crates/trusty-mpm`, Rust), which is **not yet ready**; when it
  ships it becomes the native orchestrator member.
- Consequently the versioned tool contract + stack manifest (DOC-1/DOC-2) and the
  conformance/adapter doc (DOC-6) **must treat the orchestrator as swappable
  (claude-mpm now → trusty-mpm later) without controller changes**. The name and
  architecture chosen here deliberately do not bind the controller to any single
  orchestrator.

**Follow-ups / open items:**

- Add `tctl → trusty-controller` to the abbreviation table in the root
  `CLAUDE.md` (not done in this commit).
- The crate's **license is unresolved**: `Elastic-2.0` (like trusty-search) vs
  `MIT` (like trusty-memory / trusty-analyze). Elastic-2.0 is the workspace
  default; the choice is recorded as open in DOC-0 and is out of scope for this
  ADR.
