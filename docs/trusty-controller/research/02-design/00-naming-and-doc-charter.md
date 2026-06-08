# DOC-0 — Naming & Documentation Charter

**Status:** Accepted
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Decide the tool's real name (binary, crate, directory) and lock the
refinement-doc conventions used across this design set.

## Decisions

### Naming

- **Crate name:** `trusty-controller`
- **Directory:** `crates/trusty-controller/`
- **Binary on PATH:** `tctl`
- **Abbreviation/alias:** `tctl` — for the abbreviation table in the root
  `CLAUDE.md`.
  - *Follow-up:* add `tctl → trusty-controller` to the abbreviation table in the
    root `CLAUDE.md` (not done in this commit; tracked in TODO below).

The naming decision is hard to reverse and is therefore recorded as an ADR (see
charter B2 and Dependencies). The candidates `trusty-ctl`, `trusty-installer`,
and reusing `trusty-tools` were considered and rejected; `trusty-controller`
(crate) + `tctl` (binary) won. Full rationale lives in the ADR.

### Scope framing (A4)

- `trusty-controller` is the **control plane for the ENTIRE claude-mpm stack**,
  not just trusty-tools. The `trusty-` prefix is retained.
- The **"orchestrator" is a pluggable stack member**, not hard-wired:
  - **claude-mpm** is the current stable orchestrator (Python, external repo) and
    must be coordinated today via a Python contract adapter — this is DOC-6's
    concern.
  - **trusty-mpm** (`crates/trusty-mpm`) is the planned in-house Rust replacement,
    **NOT YET READY**. When it ships it becomes the native orchestrator member.
- **Forward-compatibility requirement:** the contract + manifest (DOC-1/DOC-2)
  and the conformance/adapter doc (DOC-6) must treat the orchestrator as
  **swappable** (claude-mpm now → trusty-mpm later) without controller changes.

### Publishing (A5)

- `trusty-controller` is **published to crates.io** (the average user installs via
  `cargo install`), using the **same manual-publish workflow as trusty-search /
  trusty-memory** per the CLAUDE.md release convention:
  1. bump the crate version in `crates/trusty-controller/Cargo.toml`;
  2. tag `git tag trusty-controller-v<version>`;
  3. publish with `SKIP_UI_BUILD=1 cargo publish -p trusty-controller` — because
     it ships an embedded web UI (DOC-7), the `SKIP_UI_BUILD=1` prefix is
     required (the committed `ui-dist/` bundle is used; without the flag
     `build.rs` would invoke `pnpm` inside cargo's verification tarball and fail);
  4. install locally via
     `cargo install --path crates/trusty-controller --locked`;
  5. restart daemons via `launchctl bootout` / `launchctl bootstrap` (graceful
     SIGTERM restart convention).
- **License:** `Elastic-2.0` (the workspace default), declared in the crate's
  `Cargo.toml` as `license-file = "LICENSE"` (not `license = "Elastic-2.0"` —
  Elastic-2.0 is not an SPDX identifier and would be rejected at publish time;
  this matches the trusty-search convention). The crate ships a `LICENSE` file.

### Documentation charter

- **B1:** design docs follow the repo per-crate convention and live under
  `docs/trusty-controller/research/` (this move). `01-spec/` is the frozen
  source-of-record; `02-design/` is the refinement layer.
- **B2:** hard-to-reverse decisions get an **ADR** (the name → this commit's ADR;
  the contract-version scheme → a future ADR owned by DOC-1). All other decisions
  are recorded inline in their own doc.
- **B3 (defaulted — owner did not specify):** the document status lifecycle is
  `Draft → Review → Accepted → Superseded`. Each design doc's `**Status:**` line
  uses these values.
- **B4 (confirmed):** `01-spec/` stays frozen as source-of-record; `02-design/`
  is where refinement happens.

## Dependencies

### Consumes (inputs)
- Spec "Open Questions" section (the naming candidates).
- Repo abbreviation table in `CLAUDE.md`.

### Produces (consumed by)
- The chosen `<name>` is consumed by EVERY downstream doc: filenames, the binary
  declared in the manifest (DOC-2), `<tool>`-shaped contract examples (DOC-1),
  and the `.mcp.json` key / dispatch entries (DOC-5, DOC-8).

## Grounding (exists vs. net-new)

- **Exists:** the repo strongly favors short binary aliases; the `crates/*` glob
  in the root `Cargo.toml` auto-discovers any new member directory.
- **Net-new:** the name and alias are now **decided** — crate `trusty-controller`,
  dir `crates/trusty-controller/`, binary `tctl`, alias `tctl`. Still net-new
  work: creating the crate/dir, and adding the `tctl → trusty-controller` entry
  to the `CLAUDE.md` abbreviation table (follow-up below).

## Cross-cutting notes

- **Project-identity:** the chosen `<name>` will appear in project-scoped config
  keys; keep it stable since DOC-3's identity convention will reference it.

## TODO

- [x] Resolve naming
- [x] Resolve charter conventions
- [x] Add tctl alias to CLAUDE.md abbreviation table
- [x] Resolve license choice (Elastic-2.0, license-file = "LICENSE")
