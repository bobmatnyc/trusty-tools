# 0013. Rename `trusty-controller` to `trusty-installer` and build out interactive installer/upgrader

- **Status:** Accepted
- **Date:** 2026-06-26
- **Scope:** Workspace-wide (crate rename, binary rename, build-out of prebuilt-download layer, interactive component picker, self-update capability)
- **Supersedes:** ADR-0006 (`trusty-controller` naming decision)

## Context

ADR-0006 (accepted 2026-06-08) established the name `trusty-controller` for the stack control plane and `tctl` for the binary, explicitly **rejecting** the name `trusty-installer` as "too install-specific" given the tool's full scope (install, upgrade, restart, config, doctor, health).

However, **user preference has evolved:** the `install.sh` bootstrap already fronts the project as `trusty-installer`, and users have decided the install-focused name is the **right user-facing choice** regardless of the tool's internal scope. The name is the primary entry point for getting the stack onto a machine; discovery and first-impression clarity outweigh architectural purity.

**Additionally**, the tool is now being expanded with interactive component selection, prebuilt-binary downloads (Tier-1 platforms), and self-update capability. These features make it more than a headless control plane—it is becoming a **guided, interactive installer**. The expanded scope aligns the tool's purpose more closely with the `trusty-installer` name.

## Decision

We **accept ADR-0006's reasoning** (the tool is a control plane, not merely an installer) **but override the name choice** in favor of user discoverability and entry-point clarity:

- **Crate name:** `trusty-controller` → `trusty-installer`
- **Directory:** `crates/trusty-controller/` → `crates/trusty-installer/`
- **Primary binary:** `tctl` → `trusty-installer`
- **Binary alias (transition):** Retain `tctl` as an alias binary for one release cycle, with a deprecation warning
- **Abbreviation:** `trusty-installer` (primary); `tctl` (alias, deprecated)

**Build-out scope (all gated by this rename):**
- Prebuilt-binary download layer for Tier-1 platforms (macOS aarch64, Linux x86_64) using GitHub Releases
- Interactive component-picker UI (trusty-memory, trusty-search, trusty-mpm, optional claude-mpm bridge)
- Self-update capability (`trusty-installer self-update`)
- Config-dir migration: old `~/.trusty-tools/trusty-controller/` → new `~/.trusty-tools/trusty-installer/`
- macOS Developer-ID code-signing + FDA re-grant guidance for launchd daemons

The **control plane semantics remain unchanged**: the tool still manages upgrade, restart, config, doctor, and health for the entire stack (claude-mpm + trusty-tools). The name change reflects user-facing positioning, not architectural change.

## Consequences

**Easier / positive:**

- **User discoverability:** The name matches the entry point (`install.sh` → `trusty-installer`) and the primary use case (getting the stack installed on a machine).
- **Expanded scope aligns:** The new interactive installer features make the tool more than a control plane; the name fits the enriched functionality.
- **Clean transition:** The `tctl` alias provides one release cycle for users to migrate shell aliases and scripts; no breaking change.

**Harder / trade-offs:**

- **Contradicts ADR-0006:** We explicitly override a prior architectural decision. Future readers must understand the user-preference override rationale (recorded here).
- **Rename blast-radius:** ~40 test references, ~20 config/manifest references, docs dir, CI workflow, crates.io publish strategy. See SPEC-INSTALLER-01 for full table.
- **Publishing implications:** Old `trusty-controller` v0.2.0 stays published but deprecated. New `trusty-installer` is a fresh crate namespace. Existing downstream users on `trusty-controller` can migrate or stay on old version.

**Mitigations:**

- **Phase 1 PR:** Mechanical rename (all blast-radius changes) shipped as a single, reviewable changeset.
- **Build-out phases:** Phases 2–8 (prebuilt, self-update, component picker, etc.) are additive and can ship independently after rename.
- **Config migration:** Idempotent on-startup migration from old config dir to new one, with logging.
- **Deprecation cycle:** `tctl` alias persists for one release; CHANGELOG documents removal.

## Rationale: Why User Preference Overrides Architectural Purity

1. **Entry-point clarity:** Users' first interaction is `install.sh` and `trusty-installer` binary; consistency matters for discoverability.
2. **Primary use case:** The tool's strongest value proposition is **installing the stack on a new machine**; naming it after the install-specific functionality mirrors user mental models.
3. **Interactive build-out:** The new features (component picker, prebuilt downloads, self-update) reinforce the **guided installer** positioning.
4. **Low cost of change:** The rename is expensive but tractable (single-pass mechanical PR); it is cheaper now than after the tool gains more adoption.
5. **Precedent:** The workspace already separates crate names (descriptive) from binary names (short) without issue (`tga`, `tm`, `ts`); the new alias `tctl` fits this pattern.

## Open Items

- Should the old `trusty-controller` crate on crates.io be yanked, or remain published with a deprecation notice? (Defer to release process; recommend: deprecate, not yank.)
- Exactly how long should the `tctl` alias persist? (Suggested: one release cycle; can extend if users report issues.)
- Should the interactive installer be the default `trusty-installer install` behavior, or a separate `--interactive` flag? (See SPEC-INSTALLER-01 for design decision.)

## References

- **ADR-0006:** `trusty-controller` naming decision (rejected `trusty-installer`)
- **SPEC-INSTALLER-01:** Full specification for the rename + interactive installer/upgrader build-out
- **Issue #873 (macOS FDA):** Developer-ID code-signing and Full Disk Access re-grant guidance for launchd daemons
- **Release workflow:** `docs/reference/release-workflow.md` (impact on publish strategy)
