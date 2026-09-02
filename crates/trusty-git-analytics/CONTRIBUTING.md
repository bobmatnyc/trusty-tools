# Contributing to trusty-git-analytics

`trusty-git-analytics` (`tga`) is a Rust CLI tool for developer productivity analytics.
It extracts commit history, classifies work by type, and generates reports. We welcome
bug reports, documentation improvements, and new features that align with the project's
goals of accuracy, speed, and compatibility with the Python `gitflow-analytics` predecessor.

---

## Development Setup

### Prerequisites

- **Rust stable toolchain** — install from [rustup.rs](https://rustup.rs). The repo
  includes a `rust-toolchain.toml` that pins the exact toolchain version; `rustup` will
  download it automatically on first `cargo` invocation.
- No system SQLite or libgit2 required. Both are bundled: `rusqlite` links SQLite
  statically and `git2` links libgit2 statically.
- No system OpenSSL required. HTTP uses `rustls`.

### Clone and build

```bash
git clone https://github.com/bobmatnyc/trusty-tools
cd trusty-tools
cargo check -p tga
cargo test -p tga --no-fail-fast
```

The first build downloads and compiles dependencies; subsequent builds are incremental.

### Running the binary locally

```bash
# Development binary (faster to compile)
cargo run -p tga --bin tga -- analyze --config config.yaml --dry-run

# Release binary (used for benchmarking)
cargo build --release -p tga
./target/release/tga --version
```

---

## CI Gates

All pull requests must pass the following checks before merging:

```bash
# No clippy warnings
cargo clippy -p tga --all-targets -- -D warnings

# Code is formatted
cargo fmt --check

# All tests pass
cargo test -p tga --no-fail-fast
```

Run them locally before pushing:

```bash
cargo clippy -- -D warnings
cargo fmt
cargo test
```

If `cargo fmt --check` fails, run `cargo fmt` to auto-format the code.

---

## Coding Standards

### Error handling

- `anyhow::Result` in `src/main.rs` and `src/commands/`.
- `thiserror` error enums in library modules (`src/core/`, `src/collect/`,
  `src/classify/`, `src/report/`).
- No `unwrap()` or `expect()` in library code — propagate errors with `?`.
- `expect("descriptive message")` is acceptable in test code.

### Logging

Use `tracing::{info, warn, error, debug, trace}`. Never use `println!` or `eprintln!`
in library modules or command handlers.

### Async and concurrency

- All async code uses `tokio`. Do not introduce a second async runtime.
- CPU-bound batch work (commit classification) uses `rayon::par_iter()`.
- `git2` repository handles are not `Send`. Open and drop them within a single
  per-repo processing block.

### Documentation

All `pub` items in library modules (`src/core/`, `src/collect/`, `src/classify/`,
`src/report/`) must have doc comments (`///`). Binary-private code in `src/commands/`
does not require doc comments, but complex command logic should have inline comments.

---

## Commit Message Format

Follow the [Conventional Commits](https://www.conventionalcommits.org) specification:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**

| Type | When to use |
|---|---|
| `feat` | New feature |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code change that is neither a fix nor a feature |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `chore` | Build, dependencies, tooling |

**Examples:**

```
feat(classify): add ClickUp ticket pattern detection

Detects CU-NNN references in commit messages and correlates them
with the ClickUp API when fetch_on_reference is enabled.

Closes #145
```

```
fix(collect): gate ADO merge_commit_sha on merge strategy

Only populate merge_commit_sha when the ADO PR was merged via
a merge commit strategy, not squash or rebase.

Closes #96
```

When a commit resolves a GitHub issue, add `(closes #N)` in the commit body.

---

## Pull Request Process

### Before opening a PR

1. Create a GitHub issue describing the bug or feature if one does not already exist.
2. Fork the repository and create a branch from `main`:
   ```bash
   git checkout -b feat/my-feature
   ```
3. Implement your changes, add tests, and verify all CI gates pass locally.
4. Add a changelog fragment — `changelog.d/<issue-or-pr-number>-<slug>.md`, first
   line the category (`Added`/`Fixed`/`Changed`/…), then the bullet. Release time
   assembles the fragments into `CHANGELOG.md`; never edit that file by hand.

### PR guidelines

- **One issue per PR.** Keep pull requests focused on a single concern.
- **Keep diffs small.** Under 400 lines of diff is preferred. For larger changes,
  consider a sequence of smaller PRs.
- **Tests required.** New features and bug fixes must include or update tests.
- **Changelog fragment required.** Every user-visible change needs a
  `changelog.d/` fragment (see step 4 above).

### PR review

Maintainers will review within a few business days. Feedback is given as inline review
comments. Address each comment or explain why you disagree — do not silently ignore
feedback.

Once approved, a maintainer will merge the PR using a squash merge.

---

## Adding Features

Before implementing a new feature, read the
[Developer Guide](../../docs/trusty-git-analytics/developer/developer-guide.md) for architecture context. Key sections:

- [How to Add a New PM Integration](../../docs/trusty-git-analytics/developer/developer-guide.md#4-how-to-add-a-new-pm-integration)
  — for new ticket or PR data sources.
- [How to Add a New Output Format](../../docs/trusty-git-analytics/developer/developer-guide.md#5-how-to-add-a-new-output-format)
  — for new report formats.
- [Database Migrations](../../docs/trusty-git-analytics/developer/developer-guide.md#6-database-migrations)
  — for schema changes.

---

## Reporting Bugs

Open a GitHub issue with the following information:

1. **tga version**: output of `tga --version`
2. **Operating system**: e.g. "macOS 14.5 arm64", "Ubuntu 22.04 x86_64"
3. **Config snippet**: redact all tokens and credentials before pasting
4. **Command run**: exact command line invocation
5. **Expected behavior**: what you expected to happen
6. **Actual behavior**: what happened instead, including any error output

For crashes and unexpected output, run with `-vv` (debug verbosity) and include the
full output:

```bash
tga analyze --config config.yaml -vv 2>&1 | tee tga-debug.log
```

---

## Code of Conduct

This project follows the standard open source courtesy guidelines: be respectful,
assume good faith, keep discussion technical and constructive, and welcome contributors
regardless of experience level. Harassment or personal attacks will not be tolerated.
