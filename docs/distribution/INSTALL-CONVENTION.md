# trusty Installation Convention

**Single source of truth for uniform installation UX across all distributable trusty crates.**

## Purpose & Scope

This document defines the canonical installation convention that every distributable trusty binary crate must follow in its README's `Installation` section. It ensures consistent UX, messaging, and tooling support across the entire workspace.

**Distributable binary crates** (scope):
- `trusty-search` — hybrid code search daemon + MCP server
- `trusty-memory` — memory palace MCP frontend (with embedded Svelte UI)
- `trusty-analyze` — code analysis daemon + MCP server
- `trusty-review` — code review analyzer daemon + MCP server
- `trusty-mpm` — unified MPM platform (CLI binaries: `tm`, `trusty-mpm`)
- `trusty-git-analytics` (tga) — developer productivity analytics
- `trusty-code` — per-project Claude-Code orchestration harness

**Out of scope**: library-only crates, internal binaries, `publish = false` crates (e.g. `trusty-mpm-gui`), and crates that have simply never been published (e.g. `trusty-agents` — no `publish` field, no `trusty-agents-v*` release tag, zero crates.io or GitHub releases; install it with `cargo install --path crates/trusty-agents --locked` from a source checkout instead).

## Installation Channels

The workspace supports three installation channels per crate. Every distributable crate **must** present them in this order in the README:

1. **Prebuilt binaries** — GitHub Releases (macOS arm64, Linux x86_64)
2. **Cargo install** — from git source with `--locked` flag
3. **Homebrew** — self-owned tap (live; all published crates available)

### Platform Support

**Tier 1 (required for every release)**:
- **macOS arm64 (Apple Silicon)** (`aarch64-apple-darwin`) — built on GitHub Actions (apple-latest runner)
- **Linux x86_64** (`x86_64-unknown-linux-gnu`) — built on GitHub Actions (ubuntu-latest runner) via `cargo zigbuild` pinned to a glibc 2.17 baseline (the manylinux2014 floor — issue #2037), so releases run on RHEL 8/9, Debian 11/12, Ubuntu 20.04+, Amazon Linux 2/2023, etc., not just the runner's own (newer) glibc.
  - **Exception**: The ONNX-runtime crate (`trusty-search`) keeps the ORIGINAL native (non-zigbuild) x86_64 build for this leg — unchanged glibc floor, not a regression. Its default `bundled-ort` feature links a prebuilt ONNX Runtime archive compiled with the real GNU libstdc++ ABI, which zig's cross-linker (LLVM libc++, a different ABI) cannot link against; this was confirmed via a real CI dry-run while implementing #2037. Its portable-Linux option is the Tier 2 AL2023/load-dynamic asset below, which sidesteps the problem by not linking ONNX Runtime at build time at all. (`trusty-analyze` was in this set until #5067 removed its unused neural embedder; it now takes the ordinary zigbuild leg.)

**Tier 1 (continued) — Linux arm64 (issue #2037, made blocking by PR #4822)**:
- **Linux arm64** (`aarch64-unknown-linux-gnu`) — built NATIVELY on the `ubuntu-24.04-arm` runner (no longer cross-compiled from x86_64), and **required**, not `continue-on-error`: a missing arm64 asset now turns the release run red. Most crates keep the `cargo zigbuild` glibc 2.17 baseline, now applied natively. The ONNX-runtime crate (`trusty-search`) builds this leg with the load-dynamic feature set (see Tier 2) rather than default `bundled-ort`, since the bundled path has no aarch64 build-time binary to download.
  - **Caveat, measured not assumed**: for those two crates this leg uses the runner's GCC rather than zig (zig's clang miscompiles `numkong`'s AArch64 SIMD probes), so the asset carries the runner's **glibc 2.39** floor and needs OpenSSL 3 at runtime — the same floor its x86_64 sibling has always had. PR #4822 verified by execution that it runs on Ubuntu 24.04 arm64 and **fails to load on Amazon Linux 2023 arm64**. The portable arm64 answer is the Tier 2 `aarch64-linux-al2023` asset below.

**Tier 2 (optional per crate)**:
- **Amazon Linux 2023 / glibc < 2.38** — variant for the ONNX-runtime crate only (`trusty-search`; `trusty-analyze` was dropped from this set by #5067). Built with `--no-default-features --features load-dynamic` and requires runtime `ORT_DYLIB_PATH` configuration. Ships for **both architectures**: `x86_64-linux-al2023` and, since the #2533 follow-up to PR #4822, `aarch64-linux-al2023` (Graviton on AL2023). Both are built inside the `amazonlinux:2023` container on a runner of their own architecture, so they carry that distro's glibc 2.34 floor rather than the CI runner's. Both legs are non-blocking (`best_effort`), but a dropped leg is reported by the `release-completeness-audit` job rather than passing silently.
- **Apple Silicon GPU acceleration** — CoreML auto-detected at runtime for `trusty-search` and `trusty-analyze`; no build variant needed.
- **NVIDIA GPU (CUDA)** — optional feature flag; build documented separately if supported.

**Not supported**:
- macOS x86_64 (Intel) — only Apple Silicon (`aarch64-apple-darwin`) is targeted
- Windows (future consideration; not part of this convention)
- musl targets for core daemons (exception: `tga` supports musl statically)

---

## README Installation Section Template

Every distributable crate must use this **canonical template** verbatim in the `Installation` section. Replace `{{PLACEHOLDER}}` markers with crate-specific values; see the "Placeholder Values" table below.

```markdown
## Installation

### From GitHub Releases (recommended for binary users)

Prebuilt binaries are available for macOS (Apple Silicon) and Linux (x86_64).

1. Download the latest release from [GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases):
   - Look for assets tagged `{{CRATE}}-v{{VERSION}}`
   - Download the archive for your platform:
     - **macOS arm64 (Apple Silicon)**: `{{CRATE}}-v{{VERSION}}-aarch64-apple-darwin.tar.gz`
     - **Linux x86_64**: `{{CRATE}}-v{{VERSION}}-x86_64-unknown-linux-gnu.tar.gz`

2. Extract and install:
   ```bash
   tar xzf {{CRATE}}-v{{VERSION}}-*.tar.gz
   chmod +x {{BINARY}}
   sudo mv {{BINARY}} /usr/local/bin/    # or ~/.local/bin/ if you prefer user install
   ```

3. Verify the installation:
   ```bash
   {{BINARY}} --version
   ```

### From Source with Cargo

Requires Rust 1.94 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools {{CRATE}} --locked
```

This builds from the latest commit on `main` and installs the binary to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

To install a specific version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag {{CRATE}}-v{{VERSION}} {{CRATE}} --locked
```

### With Homebrew

Prebuilt bottles are available via the self-owned tap for fast, dependency-managed installation.

First, add the tap (one-time setup):

```bash
brew tap bobmatnyc/trusty
```

Then install:

```bash
brew install {{CRATE}}
```

To upgrade to the latest release:

```bash
brew upgrade {{CRATE}}
```

This provides:
- Automatic updates via `brew upgrade {{CRATE}}`
- Standard macOS / Linux PATH integration
- Optional dependency resolution (e.g., system libraries for ONNX Runtime)
- Pre-built bottles for fast installation (no compilation needed)

### Prerequisites & Special Cases

{{PREREQUISITES_SLOT}}

### Verify Installation

All installations can be verified by running:

```bash
{{BINARY}} --version
```

Expected output: the semantic version of the installed binary (e.g., `{{BINARY}} 0.4.0`).
```

---

## Placeholder Values Reference

| Placeholder | Meaning | Example | Notes |
|---|---|---|---|
| `{{CRATE}}` | Cargo crate name (from `Cargo.toml` `[package] name`) | `trusty-search` | Used in cargo install, tag patterns, crate.io links |
| `{{BINARY}}` | Binary name (from crate's `[[bin]] name`) | `trusty-search` | The executable you run; often matches crate name |
| `{{VERSION}}` | Semantic version (from `Cargo.toml` `[package] version`) | `0.4.0` | Used in GitHub Release tag, asset file names, version checks |
| `{{PREREQUISITES_SLOT}}` | Per-crate prerequisite instructions | see "Prerequisites per Crate" below | Includes system deps, env vars, daemon requirements, etc. |

---

## Prerequisites per Crate

Insert the appropriate prerequisites block in the `{{PREREQUISITES_SLOT}}` of the template above.

### trusty-search

```markdown
#### System Requirements

- **RAM**: 16 GB minimum. The daemon performs a hard check at startup and will exit with an actionable error on under-spec hosts. Set `TRUSTY_SKIP_RAM_CHECK=1` to bypass (use at your own risk).
- **Disk**: ~2 GB for the model cache (downloaded on first run to `~/Library/Caches/trusty-search/` on macOS or `$XDG_DATA_HOME/trusty-search/` on Linux).
- **OS**: macOS 12+ or Linux. Windows support is not yet available.

#### Optional: GPU Acceleration

- **macOS with Apple Silicon (M1/M2/M3/M4)**: CoreML GPU acceleration is enabled automatically. No configuration needed. The startup log will confirm: `provider=CoreML (Metal GPU / ANE)`.
- **NVIDIA GPU (CUDA)**: Install with `cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-search --features cuda --locked`. Requires CUDA toolkit installed on the host. See `CLAUDE.md` in the repository for `ORT_DYLIB_PATH` setup on Amazon Linux 2023.

#### Note: UI-Embedded Build

This crate embeds a Svelte admin UI compiled into the binary. The UI is pre-built and included in releases; no additional steps are needed to use the daemon. The `SKIP_UI_BUILD=1` environment variable only applies to CI/development workflows and should not be set by end users.
```

### trusty-memory

```markdown
#### Prerequisites

None — the daemon is self-contained and requires no external databases or configuration files to start.

#### Optional: OpenRouter API Key

The embedded memory UI includes a chat panel that requires an OpenRouter API key for the language model integration. Set `OPENROUTER_API_KEY` in your environment or enter it in the UI to enable chat features.

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
trusty-memory              # Start the daemon with chat enabled
```

Chat is optional; the daemon fully functions without it.

#### Note: Embedded Svelte UI

This crate embeds a Svelte admin UI (built and compiled into the binary). The UI is pre-built and included in releases; no additional steps are needed. The embedded UI runs on `http://127.0.0.1:<port>` — see the daemon output for the live port.
```

### trusty-analyze

```markdown
#### System Requirements

- **RAM**: 8 GB minimum (lower than trusty-search due to lighter indexing workload).
- **Disk**: ~500 MB for the model cache (downloaded on first run).
- **OS**: macOS 12+ or Linux. Windows support is not yet available.

#### LLM Configuration (optional for deep analysis)

The deep-analysis pass requires an LLM. Configure via environment variables:

```bash
# OpenRouter (default, requires API key)
export OPENROUTER_API_KEY=sk-or-v1-...
trusty-analyze start

# AWS Bedrock (optional alternative)
export TRUSTY_LLM_MODEL=bedrock/us.anthropic.claude-sonnet-4-6
export AWS_REGION=us-east-1
trusty-analyze start
```

Basic analysis (complexity, smells) runs without an LLM; the deep pass is optional.

#### No GPU or glibc variants

Since #5067 this crate links no ONNX Runtime — the neural clustering embedder
that needed it was removed. There is no CUDA variant, no Amazon Linux 2023
variant, and no `ORT_DYLIB_PATH` to set. The standard install above works on
every supported host.

#### Note: Embedded Svelte UI

This crate embeds a Svelte admin UI compiled into the binary. The UI is pre-built and included in releases; no additional steps are needed.
```

### trusty-review

```markdown
#### System Requirements

- **RAM**: 8 GB minimum.
- **Disk**: ~500 MB for the model cache (downloaded on first run).
- **OS**: macOS 12+ or Linux. Windows support is not yet available.

#### LLM Configuration (required for code review)

The code review daemon requires an LLM for analysis. Configure via environment variables:

```bash
# OpenRouter (default, requires API key)
export OPENROUTER_API_KEY=sk-or-v1-...
trusty-review start

# AWS Bedrock (optional alternative)
export TRUSTY_LLM_MODEL=bedrock/us.anthropic.claude-sonnet-4-6
export AWS_REGION=us-east-1
trusty-review start
```

#### Optional: NVIDIA GPU (CUDA)

Install with `cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review --features cuda --locked`. Requires CUDA toolkit.
```

### trusty-mpm (trusty-mpm binaries)

```markdown
#### System Requirements

- **Node.js** (optional): only needed if you plan to use the MPM JavaScript SDK or integrate with third-party JavaScript tooling. The daemon and CLI work independently of Node.
- **OS**: macOS 12+ or Linux. Windows support is not yet available.

#### Configuration

The daemon reads from `$XDG_CONFIG_HOME/trusty-mpm/config.toml`, falling back to `~/.config/trusty-mpm/config.toml`, by default. See the `trusty-mpm` crate README for configuration examples and the full option reference.

There is no separate daemon executable — the crate ships one CLI
(installed as both `tm` and `trusty-mpm`), and daemon mode is its `daemon`
subcommand:

```bash
tm daemon
```

The `daemon` subcommand has no `--config` flag; to use a config file at a
non-default location, set `XDG_CONFIG_HOME` before running it.

The CLI (`tm` / `trusty-mpm`) discovers the running daemon automatically via the standard socket or HTTP port and requires no configuration beyond a running daemon.
```

### trusty-git-analytics (tga)

```markdown
#### System Requirements

- **Git**: standard; the tool reads git history via git2.
- **OS**: macOS or Linux (Windows support via WSL2; not officially tested).
- **Database**: SQLite (bundled; no external SQLite install required).

#### Configuration

The CLI reads from `tga.yaml` or `~/.config/tga/config.yaml`. See the crate README and the configuration specification for details on setting up repository paths, identity resolvers, and report outputs.

```bash
tga analyze --config /path/to/tga.yaml
```
```

### trusty-code

```markdown
#### Prerequisites

- **Claude Code** (optional but recommended): this is a per-project orchestration harness that integrates with Claude Code's internal agent APIs. Standalone usage is not yet documented.
- **Git**: standard; the tool reads git metadata for branch context.

Configuration and usage details are documented in the `trusty-code` crate README.
```

---

## Special Case: ONNX Runtime on Amazon Linux 2023 / glibc < 2.38

For `trusty-search` on Amazon Linux 2023 or any host with glibc < 2.38.
(`trusty-analyze` no longer appears here — #5067 removed ONNX Runtime from it
along with its unused neural embedder, so it installs the standard way
everywhere.)

1. Install from source with load-dynamic linking:
   ```bash
   cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-search \
     --no-default-features --features load-dynamic --locked
   ```

2. Install a compatible ONNX Runtime (e.g., glibc 2.31):
   ```bash
   curl -L https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-x64-1.20.1.tgz \
     | sudo tar xz -C /opt
   ```

3. Point the daemon to the installed library:
   ```bash
   export ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so
   trusty-search start
   ```

This allows the daemon to run on newer systems where the bundled ORT library (glibc ≥ 2.38) is not available.

---

## Homebrew Installation (Live)

### Current Implementation

Homebrew distribution is provided via the **self-owned tap** (`bobmatnyc/homebrew-trusty`), which is now **live and actively maintained**. The tap provides:

- **Full control** over release timing, bottle curation, and dependency management.
- **Fast iteration** for patch releases and platform-specific variants.
- **Public repository**: [bobmatnyc/homebrew-trusty](https://github.com/bobmatnyc/homebrew-trusty)
- **Automatic bottle updates**: CI automation (`HOMEBREW_TAP_ENABLED=true`) in `.github/workflows/release.yml` bumps formulas and bottles on every real tag push (issues #896, #902).

### Published Formulas

The following crates are available on the tap with auto-bumped, pre-built bottles for macOS arm64 and Linux x86_64:

- `trusty-search` (+ `trusty-embedderd`, `trusty-console` bundled)
- `trusty-memory`
- `trusty-analyze`
- `trusty-mpm`
- `trusty-review`
- `trusty-git-analytics` (tga)

**Note**: `trusty-code` (`tcode`) and `trusty-installer` (`tctl` transitional alias) are not yet on the tap; they will be added as those crates reach release maturity.

### Installation UX

```bash
# One-time setup
brew tap bobmatnyc/trusty

# Install (uses prebuilt bottle; no compilation needed)
brew install trusty-search        # or trusty-memory, trusty-analyze, etc.

# Update to latest release
brew upgrade trusty-search

# Show installed version and info
brew info trusty-search

# Uninstall
brew uninstall trusty-search
```

### Release Workflow Integration

When a new release tag is pushed (`<crate>-v<version>`):

1. GitHub Actions builds release binaries for macOS arm64 and Linux x86_64.
2. The `.github/workflows/release.yml` `homebrew-bump` job (gated by `HOMEBREW_TAP_ENABLED=true`) automatically:
   - Creates a PR to `bobmatnyc/homebrew-trusty` with the updated formula version and bottle checksums.
   - Uses `HOMEBREW_TAP_TOKEN` (GitHub personal access token) for authentication.
3. The tap's CI validates and merges the formula update.
4. Users fetch the new bottle on next `brew upgrade` or `brew install`.

**Future path**: Homebrew Core submission (`homebrew/core`) is unblocked by the MIT relicense (issue #898) and remains a possible longer-term path for higher discoverability, pending demand and maintainability assessment.

---

## Adoption Checklist

Use this checklist to verify a crate conforms to the INSTALL-CONVENTION:

- [ ] **README.md has an "Installation" section** with the exact subsections in the canonical template order:
  - [ ] From GitHub Releases
  - [ ] From Source with Cargo
  - [ ] With Homebrew
  - [ ] Prerequisites & Special Cases (crate-specific callout)
  - [ ] Verify Installation

- [ ] **Placeholders are filled in**:
  - [ ] `{{CRATE}}` replaced with actual crate name (from `Cargo.toml` `[package] name`)
  - [ ] `{{BINARY}}` replaced with actual binary name (from `[[bin]] name` or derived from crate name)
  - [ ] `{{VERSION}}` replaced with actual version from `Cargo.toml` (e.g., `0.4.0`)
  - [ ] `{{PREREQUISITES_SLOT}}` replaced with crate-specific prerequisites or removed if none apply

- [ ] **GitHub Release binaries are published** for each release tagged `<crate>-v<version>`:
  - [ ] macOS arm64 (Apple Silicon) asset available (`aarch64-apple-darwin.tar.gz`)
  - [ ] Linux x86_64 asset available (`x86_64-unknown-linux-gnu.tar.gz`)
  - [ ] Each asset is a `.tar.gz` containing the binary and optional docs

- [ ] **Cargo install works**:
  - [ ] `cargo install --git https://github.com/bobmatnyc/trusty-tools <crate> --locked` succeeds
  - [ ] Installed binary runs with `--version` flag

- [ ] **Verification command runs**:
  - [ ] `{{BINARY}} --version` outputs the semantic version

- [ ] **No proprietary or internal tooling mentioned** in the Installation section
  - [ ] All tools and services referenced are publicly available or optional

---

## Release Workflow Requirements

Distributable crates **must** have a GitHub Actions workflow that:

1. **Triggers on tag push** matching the pattern `<crate>-v<version>` (e.g., `trusty-search-v0.4.0`)
2. **Builds for Tier 1 platforms**:
   - macOS arm64 (Apple Silicon) (`aarch64-apple-darwin`)
   - Linux x86_64 (`x86_64-unknown-linux-gnu`)
3. **Creates GitHub Release** with platform-specific binaries as `.tar.gz` assets
4. **Computes SHA256** hashes for each asset and includes them in the release notes or a companion file
5. **Publishes to crates.io** (if applicable; libraries skip this; UI-embedding crates use `SKIP_UI_BUILD=1`)

This repo's distributable crates share ONE data-driven workflow rather than a
per-crate copy: `.github/workflows/release.yml` (`Binary Release`, triggered on
`*-v*` tag pushes). See its `trusty-git-analytics` branch of the per-crate
`CRATE_CONFIG` table (in the `Resolve per-crate config` step) for a worked
example of requirements 1–4. Requirement 5, publishing to crates.io, is a
separate manual step run by a human — see `Skill(skill="cargo-publish")` and
[docs/reference/release-workflow.md](../reference/release-workflow.md) — not
something this workflow does itself.

---

## Notes for Maintainers

### When Adding a New Distributable Crate

1. Add a config branch for the new crate to the shared `.github/workflows/release.yml` `CRATE_CONFIG` table (see that file's own "TO ADD A NEW CRATE" comment) — do not create a separate per-crate workflow.
2. Add an entry to the **Placeholder Values** table above.
3. Create the crate-specific **Prerequisites** section if needed.
4. Add the crate to the "Distributable binary crates" list in the Scope section.
5. Build and test a release locally: tag, push, verify the workflow runs, download and test the binary.

### When Updating This Convention

Changes to the canonical template, placeholder requirements, or platform matrix **must** be:
1. Documented in this file with a change summary.
2. Rolled out to all distributable crates in a single PR (or coordinated across PRs).
3. Validated by spot-checking at least two crates' READMEs and `cargo install` attempts.

---

## Appendix: Historical Context

Prior to this convention:
- Installation instructions were scattered across crate READMEs with inconsistent wording.
- No single place documented the platform matrix or Homebrew plans.
- Each crate had bespoke release workflows with subtle differences.

This document consolidates the scattered practices into a single, canonical form to reduce maintenance burden and improve UX consistency across the entire workspace.
