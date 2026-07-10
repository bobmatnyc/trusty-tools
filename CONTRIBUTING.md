# Contributing to trusty-tools

Thank you for your interest in contributing to trusty-tools! This document provides guidelines for development and contributing code back to the project.

## Getting Started

**Before contributing, read [.trusty-mpm/INSTRUCTIONS.md](.trusty-mpm/INSTRUCTIONS.md)** — it is the authoritative reference for development workflows, conventions, and internal tooling patterns.

### Prerequisites

- Rust 1.91+ (enforced by MSRV in the workspace)
- Git
- Node.js + pnpm (only if working on embedded Svelte UIs)

### Build and Test

```bash
# Build all crates (debug)
cargo build

# Build release (optimized)
cargo build --release

# Run all tests
cargo test

# Check without codegen (fast)
cargo check

# Lint everything
cargo clippy --workspace --all-targets -- -D warnings

# Format check
cargo fmt --check

# Format and fix
cargo fmt
```

For single-crate workflows, use `-p <crate-name>` to scope commands. See `.trusty-mpm/INSTRUCTIONS.md` for the full command reference.

## Worktree Discipline

All code changes happen in **dedicated git worktrees** branched off `origin/main`. This protects the main checkout from mutation and allows safe parallel work.

```bash
# Create a worktree for your feature/fix
git fetch origin main
git worktree add -b <feature-branch> .claude/worktrees/<dirname> origin/main
cd .claude/worktrees/<dirname>

# … edit, build, test, commit from here …

# After PR merge, clean up
git worktree remove --force <path>
git push origin --delete <feature-branch>
```

**Note on worktree paths:** The `.claude/worktrees/<dirname>` path is an internal convention used by Claude Code and automated tooling. External contributors may use any worktree location they prefer (e.g., `../trusty-tools-worktrees/<dirname>` or a sibling directory).

**Important:** Never use `git reset --hard`, `git checkout .`, or `git stash` against the main checkout. The main repo is inspection-only.

## Code Conventions

### SLOC File Size Limits

Enforced per-crate, dual-cap (production vs. test):

| File Type | Cap | Enforcement |
|---|---|---|
| Production source (`.rs` in `src/`) | **500 SLOC** | Mechanical gate in CI + pre-commit hook |
| Test / benchmark files | **1500 SLOC** | Mechanical gate in CI |

When a file approaches its limit, split it into focused submodules before adding more features. Run:

```bash
bash scripts/check_line_cap.sh        # Check current status
bash scripts/check_line_cap.sh --update  # Ratchet down after intentional splits
```

### Error Handling

- **Library crates:** Use `thiserror::Error` for structured error enums
- **Application/binary crates:** Use `anyhow::Result` for ergonomic error handling
- **Never** use `unwrap()` in library code; reserve `expect()` for genuine invariants only

### Logging

- Log to **stderr only** via `tracing` macros — stdout must stay clean for MCP JSON-RPC framing
- Never log to stdout in daemons or MCP servers

### Documentation

Every public item (function, struct, trait, module) must carry three comment sections:

```rust
/// Why: <motivation — the problem this solves>
/// What: <mechanical description of what it does>
/// Test: <where coverage lives, or why untestable>
pub fn my_function() { … }
```

## Commit Message Format

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<body>

Closes #<issue>
```

**Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `chore`, `docs`

**Examples:**
- `feat(trusty-search): add branch-aware query routing (#456)`
- `fix(trusty-memory): resolve race condition in concurrent writes`
- `refactor(trusty-agents): extract validation logic to separate module`
- `perf(trusty-analyze): optimize KG expansion with caching`

## Per-Crate Versioning & Release

Each crate independently manages its version in its `Cargo.toml`. There is no workspace-level version.

**Release workflow:**

1. Bump `version` in `crates/<name>/Cargo.toml`
2. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`
3. Commit: `version: bump <name> to <version>`
4. Create git tag: `git tag <crate-name>-v<version>`
5. Push tag: `git push origin <crate-name>-v<version>`
6. Publish: `cargo publish -p <crate-name>`

For UI-embedding crates, use `SKIP_UI_BUILD=1 cargo publish`.

See [docs/reference/release-workflow.md](docs/reference/release-workflow.md) for macOS-specific notes and the full convention.

## Pull Request Process

1. **Create a worktree and feature branch** (see Worktree Discipline above)
2. **Make your changes** following the conventions above
3. **Test locally:** `cargo test --workspace`
4. **Lint:** `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt`
5. **Check line caps:** `bash scripts/check_line_cap.sh` (exit 0 = clean)
6. **Commit with a conventional message** (see Commit Message Format)
7. **Push to origin:** `git push -u origin <feature-branch>`
8. **Open a PR** linking any related issues
9. **Wait for trusty-review + CI** (all checks must pass)

## License

All contributions are licensed under the MIT License. By contributing, you agree to license your work under MIT.

---

For detailed development information, references, and troubleshooting, consult [.trusty-mpm/INSTRUCTIONS.md](.trusty-mpm/INSTRUCTIONS.md).
