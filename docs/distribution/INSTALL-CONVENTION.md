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

**Out of scope**: library-only crates, internal binaries, and publish=false crates (trusty-mpm-gui, trusty-agents, etc.).

## Installation Channels

The workspace supports three installation channels per crate. Every distributable crate **must** present them in this order in the README:

1. **Prebuilt binaries** — GitHub Releases (macOS arm64, Linux x86_64)
2. **Cargo install** — from git source with `--locked` flag
3. **Homebrew** — planned; not yet available

### Platform Support

**Tier 1 (required for every release)**:
- **macOS arm64 (Apple Silicon)** (`aarch64-apple-darwin`) — built on GitHub Actions (apple-latest runner)
- **Linux x86_64** (`x86_64-unknown-linux-gnu`) — built on GitHub Actions (ubuntu-latest runner)

**Tier 2 (optional per crate)**:
- **Amazon Linux 2023 / glibc < 2.38** — variant for ONNX-runtime crates only (`trusty-analyze`). Built with `--no-default-features --features http-server,load-dynamic` and requires runtime `ORT_DYLIB_PATH` configuration.
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

Requires Rust 1.91 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools {{CRATE}} --locked
```

This builds from the latest commit on `main` and installs the binary to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

To install a specific version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag {{CRATE}}-v{{VERSION}} {{CRATE}} --locked
```

### With Homebrew (planned — not yet available)

```bash
brew tap bobmatnyc/trusty
brew install {{CRATE}}
```

This installation method is under development. For now, use GitHub Releases or `cargo install`.

Once available, this will provide:
- Automatic updates via `brew upgrade {{CRATE}}`
- Standard macOS / Linux PATH integration
- Optional dependency resolution (e.g., system libraries for ONNX Runtime)

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

#### Optional: NVIDIA GPU (CUDA)

Install with `cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --features cuda --locked`. Requires CUDA toolkit. On Amazon Linux 2023 and other glibc < 2.38 hosts, use the load-dynamic build:

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze \
  --no-default-features --features http-server,load-dynamic --locked

# Point to system ONNX Runtime
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
trusty-analyze start
```

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

The daemon reads from `~/.config/trusty-mpm/config.yaml` by default. See the `trusty-mpm` crate README for configuration examples and the full option reference.

```bash
trusty-mpmd --config /path/to/config.yaml
```

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

For `trusty-analyze` (and `trusty-search` with CUDA) on Amazon Linux 2023 or any host with glibc < 2.38:

1. Install from source with load-dynamic linking:
   ```bash
   cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze \
     --no-default-features --features http-server,load-dynamic --locked
   ```

2. Install a compatible ONNX Runtime (e.g., glibc 2.31):
   ```bash
   curl -L https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-x64-1.20.1.tgz \
     | sudo tar xz -C /opt
   ```

3. Point the daemon to the installed library:
   ```bash
   export ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so
   trusty-analyze start
   ```

This allows the daemon to run on newer systems where the bundled ORT library (glibc ≥ 2.38) is not available.

---

## Homebrew Installation (Planned Future State)

### Design

Homebrew distribution will be provided via **one of two paths**, each with distinct tradeoffs:

**Option A: Self-Owned Tap** (`bobmatnyc/homebrew-trusty`)
- Full control over release timing, bottle curation, and dependencies.
- Faster iteration for patch releases and platform-specific variants.
- Users must explicitly `brew tap bobmatnyc/trusty` once, then `brew install` as normal.
- **Likely the near-term pragmatic choice** for rapid cadence and experimental features.

**Option B: Homebrew Core** (`homebrew/core`)
- MIT is OSI-approved, so homebrew-core is now eligible (ELv2 blockage removed).
- No user tap setup required — direct `brew install trusty-search` from core formulae.
- Longer review latency (core maintainers vet all PRs); slower time-to-release for patches.
- Higher visibility and discoverability; native macOS/Linux user expectation.

Both approaches use **bottles (prebuilt binaries)** so end users avoid compilation time. Fallback to building from source is included for unrepresented platforms.

**Current direction**: A self-owned tap is planned first (Phase 2) for faster iteration and decoupling from core-review cycles. Migration to homebrew-core can follow once the tap is stable and the community demonstrates demand.

### Intended UX (when available)

```bash
# One-time setup
brew tap bobmatnyc/trusty

# Install (uses prebuilt bottle if available)
brew install trusty-search        # or trusty-memory, trusty-analyze, etc.

# Update to latest release
brew upgrade trusty-search

# Show installed version
brew info trusty-search

# Uninstall
brew uninstall trusty-search
```

### Release Workflow Integration (Implementation Road Map)

Once a chosen path is created, the release workflow will:

1. Build GitHub Release binaries (as today).
2. Trigger a webhook or automated PR to the tap (self-owned or core) to bump the formula's version and bottle checksums.
3. The tap's CI will build and test bottles for macOS (arm64) and Linux (x86_64).
4. Users run `brew upgrade` to fetch the new bottle on next run.

**Current status**: Planned for Phase 2. The GitHub Release infrastructure is ready; the Homebrew automation is not yet in place. The self-owned tap is the expected first implementation.

---

## Adoption Checklist

Use this checklist to verify a crate conforms to the INSTALL-CONVENTION:

- [ ] **README.md has an "Installation" section** with the exact subsections in the canonical template order:
  - [ ] From GitHub Releases
  - [ ] From Source with Cargo
  - [ ] With Homebrew (planned)
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

See `crates/trusty-git-analytics/.github/workflows/release.yml` for a worked example.

---

## Notes for Maintainers

### When Adding a New Distributable Crate

1. Create a GitHub Actions release workflow (copy from `tga` and customize).
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
