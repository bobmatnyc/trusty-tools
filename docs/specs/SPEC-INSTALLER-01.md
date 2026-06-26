# SPEC-INSTALLER-01: trusty-installer Rename & Interactive Installer/Upgrader

**Specification ID:** SPEC-INSTALLER-01  
**Status:** Draft  
**Date:** 2026-06-26  
**Author:** Claude Code (Haiku 4.5)  
**Supersedes:** —  
**Lifecycle Stage:** Design (public review pending)

---

## 1. Summary

Rename `trusty-controller` (binary `tctl`, crate v0.2.0, `crates/trusty-controller/`) to `trusty-installer` and build out an **interactive installer/upgrader** with component selection, prebuilt-binary downloads, and self-update capability. The rename reflects the tool's primary user entry point (the `install.sh` bootstrap) and its role as the discoverable installation point for the entire trusty-* stack. The build-out extends the headless control plane (install, upgrade, restart, health, config, doctor) into an interactive guided experience for first-time users.

---

## 2. Motivation

**Why rename?**

- **User discovery:** The bootstrap `install.sh` already names the crate `trusty-installer`; the binary name should match for consistency and discoverability.
- **Entry-point clarity:** The tool is the user's primary interaction for **getting the stack onto a machine**; it is the installer/entry point, even though it also manages upgrades and daemon lifecycle.
- **ADR-0006 revisited:** ADR-0006 rejected "trusty-installer" as too install-specific, but the user has decided the install-focused name is the right **user-facing** choice regardless. The tool's full scope (upgrade, doctor, health) remains; we acknowledge ADR-0006's reasoning in ADR-0013 and explain the name change.

**Why build it out now?**

- **Tier-1 prebuilt downloads:** Today `tctl install/upgrade` uses **only `cargo install`** (slow, requires Rust toolchain). Net-new work: add prebuilt-binary download layer (GitHub Releases asset discovery, platform/arch detection, SHA-256 verification, tarball extraction) on Tier-1 platforms.
- **Interactive component picker:** New users need a guided choice of what to install (trusty-memory, trusty-search, trusty-mpm, optional claude-mpm integration). This is the **unified installer UX**.
- **Self-update capability:** Like `rustup self update`, the installer can download and swap its own binary.
- **Config-dir migration:** Existing users' persistent state (`~/.trusty-tools/trusty-controller/{config.yaml,ensure.lock}`) must migrate to the new dir (`…/trusty-installer/`).

---

## 3. Goals & Non-Goals

### Goals

- Rename `trusty-controller` → `trusty-installer` crate and `tctl` → `trusty-installer` binary (retain `tctl` alias for transition).
- Implement prebuilt-binary download layer for Tier-1 platforms (macOS aarch64, Linux x86_64).
- Fall back to `cargo install` for unsupported platforms with clear messaging + Rust-toolchain check.
- Add interactive component-picker UI (memory, search, trusty-mpm, claude-mpm bridge).
- Implement config-dir migration (old `trusty-controller/` → new `trusty-installer/` with fallback copy).
- Self-update capability: `trusty-installer` can update its own binary.
- Extend upgrade flow to prebuilt path (GitHub Release tag comparison, download+verify+swap).
- Validate external prerequisites (claude, git, tmux on PATH) at startup.
- Handle macOS Developer-ID code-signing handoff for `trusty-search` (reuse `scripts/install-trusty-search-signed.sh` pattern).
- Handle macOS FDA (Full Disk Access) grant guidance for launchd daemons (issue #873).
- Update all blast-radius references (Cargo.toml, CLI, tests, docs, CI).

### Non-Goals

- Yank the old `trusty-controller` crate from crates.io (optional deprecation only; out of scope).
- Widen the `tctl` alias transition period beyond one release cycle.
- Add other install methods (Homebrew direct, Docker, etc.) — stay scoped to GitHub Releases + cargo fallback.
- Refactor the 5-stage `up` boot orchestrator (remains as-is).
- Change the 3-tier supervisor/daemon architecture (remains as-is).

---

## 4. Architecture

### 4.1 Two-Stage Bootstrap

**Stage 1:** Root `install.sh` (shell script)
- Platform/arch detection (`uname -m`, `uname -s`)
- Downloads prebuilt `trusty-installer-<version>-<target>.tar.gz` from GitHub Releases
- Downloads + verifies SHA-256 (`trusty-installer-<version>-<target>.tar.gz.sha256`)
- Extracts to `~/.local/bin/trusty-installer` (or user PATH override)
- Invokes `trusty-installer install` (Stage 2)
- Updates `CRATE="trusty-installer"` and `BIN="trusty-installer"` constants (from current `CRATE="trusty-controller"`, `BIN="tctl"`)

**Stage 2:** `trusty-installer` binary (interactive installer)
- Renders component-picker UI (existing tctl table rendering + selection)
- Validates external prerequisites (claude, git, tmux)
- Performs platform-specific setup per chosen component
- Wires launchd plists, MCP socket, supervisor, etc.
- Prompts for macOS code-signing + FDA re-grant if applicable
- Saves config to new `~/.trusty-tools/trusty-installer/` directory

---

## 5. Component Matrix

| Component | Binaries Bundled | Setup Command | Launchd Plist | macOS Concerns |
|---|---|---|---|---|
| **memory** | `trusty-memory`, `trusty-bm25-daemon` | `trusty-memory setup` | Auto-generated from template, bootstrapped | FDA not typically needed (reads model files, writes to `~/.trusty-tools/`) |
| **search** | `trusty-search`, `trusty-embedderd` | `trusty-search service install` | Auto-generated, bootstrapped | **Developer-ID signing required** (issue #873 context); installer calls `scripts/install-trusty-search-signed.sh`; prompts for FDA re-grant after install |
| **trusty-mpm** | `tm`, `trusty-mpm` (supervisor not binary) | `tm install` + plist bootstrap | `crates/trusty-mpm/deploy/supervisor/com.trusty.mpm.supervisor.plist` (fill placeholders + `launchctl bootstrap`) | `tm install` does NOT bootstrap supervisor plist; installer must do it |
| **claude-mpm bridge** | None | MCP/hook wiring only | N/A (claude-mpm manages its own launchd if any) | Detect existing install, wire trusty-* servers into claude-mpm's `.mcp.json` or config if applicable |

---

## 6. Build-Out Work Items

The work breaks into 8 independent phases (can ship in any order after Phase 1):

**Phase 1:** Mechanical rename (crate dir, Cargo.toml, imports, CLI, docs).  
**Phase 2:** Prebuilt-download layer (GitHub asset discovery, SHA-256 verify, platform detection, cargo fallback).  
**Phase 3:** Self-update command (atomic binary swap, extend upgrade flow).  
**Phase 4:** Interactive component picker + claude-mpm integration.  
**Phase 5:** Config-dir migration + startup (idempotent old→new state move).  
**Phase 6:** External prerequisite validation (claude, git, tmux checks).  
**Phase 7:** Supervisor plist bootstrap (template fill, launchctl invocation).  
**Phase 8:** macOS code-signing + FDA guidance (cert detection, script invocation).  

---

## 7. Rename Blast-Radius

**Key changes:**

- Crate dir: `crates/trusty-controller/` → `crates/trusty-installer/`
- Package name in Cargo.toml: `trusty-controller` → `trusty-installer`
- Lib name: `trusty_controller` → `trusty_installer`
- Add `[[bin]] trusty-installer` + keep `[[bin]] tctl` as alias (one release cycle)
- All `use trusty_controller::` → `use trusty_installer::`
- CLI primary name: `trusty-installer` (tctl becomes alias)
- `install.sh` constants: `CRATE="trusty-installer"`, `BIN="trusty-installer"`
- Manifest/config paths: `.join("trusty-controller")` → `.join("trusty-installer")`
- Upgrade self-guard: `"trusty-controller"`/`"tctl"` → `"trusty-installer"`
- CI workflow: asset naming, CARGO_PKG, Homebrew gate
- Docs dir: `docs/trusty-controller/` → `docs/trusty-installer/` (via `git mv`)
- CLAUDE.md: add `trusty-installer → -p trusty-installer`, keep `tctl → trusty-installer` alias
- ~40 test references to binary name + crate name

**Process:**
1. Phase 1 PR: all mechanical changes
2. Phase 2–8 PRs: additive features (don't depend on rename)
3. Single CI/docs/publish pass

---

## 8. Transition & Deprecation

**Binary alias:** One release cycle (e.g., v0.1.0 → v0.2.0). After that, remove `tctl` bin and document in CHANGELOG.  
**Config migration:** Idempotent on every startup (check old dir exists, new doesn't, then move). Log to user.  
**crates.io:** Publish NEW `trusty-installer` crate. Old `trusty-controller` stays published but marked deprecated.  

---

## 9. Success Criteria

- Rename: All references updated, tests pass, docs in new location.
- Prebuilt: <5 sec download on Tier-1; graceful fallback with toolchain check.
- Component picker: Guided UX, transitive-dep resolution, claude-mpm optional.
- Self-update: Atomic binary swap, clean exit.
- Config migration: Automatic on first run, idempotent, logged.
- macOS: Developer-ID + FDA guidance integrated; users can use search daemon post-install.
- Docs: ADR-0013 explains reasoning; spec clear; CLAUDE.md abbrev updated.

---

**Document History:**
- 2026-06-26: Draft created, all LOCKED decisions embedded.
