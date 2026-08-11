# VM Install-Testing Harness — Track B (independent research)

**Author:** Research (Track B, independent — no coordination with Track A)
**Date:** 2026-07-30
**Status:** Research / design recommendation. No code written. No VMs started.
**Scope:** Ad-hoc, manually-invoked installation testing of `trusty-tools` in ephemeral
Tart macOS VMs on the local Apple Silicon host.

---

## 1. Executive summary + headline recommendation

- **Use raw `tart` driven by shell, not Cirrus CLI.** Cirrus's Tart integration is real and
  its `.gitignore`-aware rsync is genuinely attractive, but it takes over VM lifecycle,
  deletes the VM on completion (killing post-mortem debugging), offers no host-directory
  mount for macOS guests, and — decisively — trips macOS 15+/Tahoe's **"Local Network"
  permission**, which requires either a GUI popup accept per host or running `cirrus run`
  as `root --user mac`. That is unacceptable friction for an ad-hoc maintainer tool.
  Raw `tart` + `tart exec` avoids the network path entirely (vsock).
- **Use `tart exec`, not SSH.** The `macos-tahoe-base` image ships `tart-guest-agent` as a
  LaunchAgent with `--run-rpc`, which is exactly what `tart exec` and
  `tart ip --resolver=agent` need. This gives non-interactive command execution over
  **vsock** — no IP, no host keys, no password, no `sshpass`, no Local Network permission.
  Keep SSH as a *debug-only* fallback via one-shot pubkey injection through `tart exec -i`.
- **The harness is bash, living at `scripts/vmtest/` — never under `crates/`.** Root
  `Cargo.toml:15` declares `members = ["crates/*"]`, so *any* directory placed under
  `crates/` automatically joins the workspace and is therefore swept up by
  `cargo test --workspace`, `cargo clippy --workspace --all-targets -D warnings`
  (`Makefile:46-49`), `scripts/check_line_cap.sh` (500-SLOC cap on tracked `.rs`),
  `check_test_pointers.sh`, `check_sld.sh`, and `publish-dry-run-order.sh`. A Rust harness
  crate would directly violate the "separate from existing test infrastructure"
  requirement. Shell has none of these entanglements and matches the existing
  `tests/*.sh` + `scripts/*.sh` culture. This also matches the already-Accepted
  DOC-10 recommendation (`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md:525-527`:
  *"Rust is awkward for tart/docker orchestration; shell is the right tool"*).
- **Two-stage image strategy: `tahoe-base` → `trusty-toolchain-<tag>` golden → per-run
  ephemeral clone.** Prebaking mise + Rust 1.91 + uv + gh does **not** violate "clean VM"
  because the golden contains zero trusty-tools artifacts (no `~/.cargo/registry` entries
  for trusty crates, no binaries, no `~/.trusty-*`, no launch agents, no `.mcp.json`).
  Saves ~3–8 min/run. Keep a `--from base` escape hatch so the *missing-toolchain*
  guide-and-abort path can still be exercised.
- **Local-source transfer: read-only virtiofs mount of a host *staging* dir containing a
  `git ls-files`-derived tarball — never a read-write mount of the repo.** The repo tree is
  **29 GB** (`.claude/` alone is 21 GB of worktrees, `target/` is 6.6 GB), while the true
  working set is **85 MB / 5,306 files**. A read-write mount would let the guest's
  `cargo build` and the UI `build.rs` placeholder writers scribble into the host tree —
  that is a host effect and disqualifies the naive approach.

---

## 2. Findings: existing install machinery in this repo

### 2.1 There is already an Accepted design doc for exactly this

`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md` (692 lines,
**Status: Accepted (owner-approved)**) is the capstone isolation-harness design. It
**already picked `tart`** as the canonical local macOS tool
(`10-isolation-testing-harness.md:193-213`, decision ratified at `:651-654`), already
resolved the launchd-per-uid isolation argument (`:170-192`), already specified the
provision/teardown lifecycle as `tart clone` → `tart run` → `tart stop && tart delete`
(`:495-499`), and already recommended **shell for VM lifecycle** (`:525-527`).

**Track-B position:** adopt DOC-10's verdicts wholesale where they overlap, and *diverge*
on one point — DOC-10 §7.3 (`:517-524`) puts the *assertions* in a Rust `#[ignore]`
integration test at `crates/trusty-controller/tests/isolation_harness.rs`. That crate name
no longer exists (it shipped as `crates/trusty-installer`), and more importantly the
current brief explicitly requires the harness be **separate from `cargo test`**. Putting
assertions in a `#[ignore]` test inside a workspace member does *not* satisfy that — the
file still gets compiled by `cargo test --workspace` and linted by `clippy --all-targets`.
Track B therefore recommends **assertions in shell too**, colocated with the driver.

DOC-10 also names two properties this harness must preserve, both load-bearing:

- **`:170-192` — a clean `$HOME` is NOT isolation on macOS.** launchd agents load into
  `gui/$(id -u)`, keyed on **uid**, not `$HOME`. Only a separate login session (i.e. a VM)
  isolates them. Confirmed empirically on this host: `~/Library/LaunchAgents/` contains
  `com.trusty.memory.plist` and `com.trusty.mpm.supervisor.plist`.
- **`:246-259` — the cdhash trap.** Never `cp` a binary into PATH on macOS; `cargo install`'s
  atomic rename keeps the kernel cdhash cache consistent, a `cp` can SIGKILL the next exec
  with `EXC_CRASH/CODESIGNING`. Reiterated in `.trusty-mpm/INSTRUCTIONS.md:412` ("🔴 CRITICAL
  macOS note: Never use `cp` to install release binaries on macOS") and `:593-594`.
  **The harness must therefore never copy prebuilt binaries from host into guest PATH.**

### 2.2 `install.sh` (784 lines, POSIX `sh`) — a bootstrap, not the installer

`install.sh` does **not** install the trusty tools. It downloads one prebuilt
`trusty-installer` tarball from GitHub Releases, SHA-256-verifies it, places
`trusty-installer` + `tctl` into `~/.local/bin`, then delegates to
`trusty-installer install` (`install.sh:592-621`).

| Aspect | Evidence |
|---|---|
| Flags | `-y`/`--yes` (`:701`), `--force` (`:704`), `-h`/`--help` (`:707`); unknown arg → `die` (`:718-720`) |
| Env | `TRUSTY_VERSION`, `TRUSTY_INSTALL_DIR` (default `$HOME/.local/bin`, `:52`), `TRUSTY_YES`, `TRUSTY_NO_MODIFY_PATH`, `TRUSTY_FORCE`, `GITHUB_TOKEN`/`GH_TOKEN`, `TRUSTY_INSTALL_SOURCE_ONLY` (test hook, `:782`) — documented `:21-31` |
| Source modes | **crates.io: no. git branch: no. local path: no.** Release-tarball only (`:511-516`), triples `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (`:65`, `:117-157`) |
| Non-interactive | No `sudo` anywhere. Non-TTY without `-y` *skips* the PATH edit with a warning (`:322-325`) rather than hanging; but `read -r _reply` (`:327`) blocks on a real TTY. **Harness must pass `-y`.** |
| Idempotency trap | `:743-752` skips re-download *and re-verification* if already current — **harness must pass `--force`** for a deterministic run |
| Verification | SHA-256 `-c` (`:526-538`); stack verification delegated, exit code propagated (`:577-582`, `:604-606`, `:769-772`) |
| Host state | Creates only `mkdir -p "${_install_dir}"` (`:547`) and appends to shell RC files (`:409-417`, `:466-478`). Everything else is a side effect of `trusty-installer install`. |

`tests/test-install-path.sh` (284 lines) is a **pure unit-test suite for `install.sh`'s
PATH handling** — it sources the script with `TRUSTY_INSTALL_SOURCE_ONLY=1` into a synthetic
`$HOME` and calls `maybe_update_path` directly (`:85-89`), 9 tests at `:269-277`. It is
network-free and is **not** an install test. Its `assert_eq` harness shape (`:63`) is the
right pattern to copy for the VM assertions.

### 2.3 `crates/trusty-installer` (v0.5.0, publishable) — the real control plane

Two binaries from one `src/main.rs`: `trusty-installer` (`Cargo.toml:22-24`) and `tctl`
(`Cargo.toml:29-31`).

- **Stable set** (`src/commands/stable_set.rs:173-182`): `trusty-search` (daemon, required),
  `trusty-memory` (required), `trusty-analyze` (optional), `trusty-review` (required),
  `tga` (non-daemon, optional), `trusty-console` (optional), `trusty-mpm` (required).
  **`trusty-code`/`tcode` is not a member.**
- **Install strategy** (`src/commands/install.rs:1-40`): prebuilt-tarball-first for the three
  Tier-1 triples (`download/platform.rs:18-30`), SHA-256 verify, atomic placement into
  `~/.local/bin` (`download/mod.rs:37`), falling back to `cargo install <crate> --locked`.
  **There is no `--path` and no `--git` install mode.**
- **`ensure`** patches the project `.mcp.json` (`commands/ensure/mcp_patch.rs:30`) with
  entries for `trusty-search` (`serve`) and `trusty-memory`/`trusty-review`/`trusty-mpm`
  (`serve --stdio`) (`mcp_patch.rs:49-60`), registers the search index, creates the memory
  palace.
- **Non-TTY gate**: `tctl install` refuses without `--yes` in a non-TTY
  (`src/commands/install_gate.rs`, documented `install.rs:26-29`) → **the harness must always
  pass `-y`.** `tart exec` is non-TTY unless `-t` is given, so this fires.
- Command surface (`src/cli.rs`): `up`, `install`, `upgrade`/`update`, `updates`, `ensure`,
  `start`/`stop`/`restart`, `stack health`/`stack doctor`, `config`, `status`, `port`,
  `doctor`, `ui`, `self-update`, `sign`. Global `--json`, `--scope`, `--timeout`, `-y`.

> ⚠️ **Design trap surfaced by this finding.** `tctl install` only knows registry/prebuilt
> sources. ADR-0043 (`docs/adr/0043-cargo-bin-policy.md:85`) *codifies* "install from registry
> only, not path". So in the **branch** and **local** patterns, running `tctl install` after a
> source install would try to *overwrite* the source-built binaries with published ones,
> silently invalidating the test. **Branch/local scenarios must drive `tctl ensure` / `tctl up`
> / `tctl stack health` for wiring and verification, but never `tctl install`** (or only
> `tctl install --dry-run`, which is explicitly safe and never prompts, `cli.rs:217-226`).

### 2.4 `Makefile` — all targets

`.PHONY` at `Makefile:14`.

| Target | Line | What |
|---|---|---|
| `clean-runtime` | 27 | `rm -f tga.db trusty-analyze.facts.redb` |
| `check` | 46 | `cargo test --workspace` → `clippy --workspace --all-targets -D warnings` → `cargo fmt --check` |
| `e2e-docker` | 64 | wraps `scripts/e2e-docker.sh`; forwards `TRUSTY_{SEARCH,MEMORY,MPM,ANALYZE}_VERSION` (`:65-69`) |
| `install-search-signed` | 94 | `scripts/install-trusty-search-signed.sh` |
| `install-mpm-signed` | 120 | `scripts/install-trusty-mpm-signed.sh` |
| `publish-check` | 142 | `scripts/check-publish-ready.sh` (needs `CRATE=`) |
| `version-parity-check` | 165 | `scripts/check-version-parity.sh` |
| `publish-dry-run-order` | 187 | `scripts/publish-dry-run-order.sh` |

**There is no `make install`, no `make verify`, and no VM target.** `e2e-docker` is the only
existing install test. Reusable env knobs already established by the signed-install scripts
(`Makefile:89-93`, `:116-119`; `scripts/install-trusty-search-signed.sh:67-71`, `:125-138`):
`TRUSTY_INSTALL_VERSION` (crates.io pin), **`TRUSTY_INSTALL_PATH` (local-path install)**,
`TRUSTY_SIGN_IDENTITY`, `TRUSTY_CODESIGN_DRY_RUN=1`.

### 2.5 `scripts/e2e-docker.sh` + `docker/e2e/` — the closest analogue, and its gap

This **is** an install test — of the *crates.io compile* path only.

- `scripts/e2e-docker.sh` flags `:49-65`; docker preflight `:70-79`; build `:119-126`;
  run `:141-147` with `-e TRUSTY_SKIP_RAM_CHECK=1 -e XDG_DATA_HOME=/tmp/trusty-data -e HOME=/root`
  and a bind-mounted log dir; exits with the container's code `:149-158`.
- `docker/e2e/Dockerfile` (102 lines, `FROM rust:slim`, `:19`): apt deps
  `pkg-config libssl-dev curl procps jq build-essential cmake` (`:26-34`); **four separate
  `RUN cargo install <crate> --locked` layers** (`:51-56`, `:58-63`, `:65-70`, `:72-77`) so
  Docker caches each independently.
- **How source gets in: it doesn't.** Only two `COPY`s — a sample repo fixture (`:83`,
  deliberately not named `fixtures` because trusty-search's walker skips that name) and
  `smoke.sh` (`:86`). Everything else is from crates.io. **This is precisely the gap a VM
  harness closes: branch and local-source installs have zero coverage today.**
- `docker/e2e/smoke.sh` (391 lines) is the assertion battery to generalize:
  search daemon `/health` (`:89`), index output contains "chunks" (`:100-106`), query returns
  hits (`:112-120`); memory health (`:165-168`), palace/drawer/recall round trip (`:182-218`);
  **the single-install assertion — both `tm` AND `trusty-mpm` on PATH (`:236-243`)** —
  `--version` non-empty (`:246-252`), `tm start`/`tm status` (`:266-280`); analyze health on
  `:7879` (`:327`) and structured `analyze` output (`:341-349`).
- Timing, stated: `.github/workflows/e2e-docker.yml:44-47` — *"~15-30 minutes on a free GHA
  runner (2 vCPU)"*, `timeout-minutes: 60` (`:83`). Nightly cron `:52-54`, not on PR (`:31`).
- **Coverage gap:** covers 4 of the 6 Homebrew-distributed tools. `trusty-review`, `tga`,
  `trusty-console`, and `trusty-installer` have no install-test coverage at all.

### 2.6 Signed installers

- `scripts/install-trusty-mpm-signed.sh` — `cargo install --path <repo>/crates/trusty-mpm --locked`
  (`:210`, `:226`) then `codesign --force --options runtime --timestamp --identifier <id> --sign`
  (`:185-186`), verify `:199`, `:202`. Fixed identifiers `:91-93`. **Hard-fails without a
  Developer ID cert** (`:246-250`) unless `TRUSTY_SIGN_IDENTITY` is set. `--dry-run` /
  `TRUSTY_CODESIGN_DRY_RUN=1` prints commands without executing (`:57-59`, `:369`).
- `scripts/install-trusty-search-signed.sh` — three install modes (`:128` crates.io pin,
  `:132` `TRUSTY_INSTALL_PATH`, `:138` repo default) and **delegates signing to `tctl sign`**
  (`:14`, `:97`), rebuilding `trusty-installer` if absent (`:174`).
- Canonical table: `crates/trusty-installer/src/commands/macos_signing/mod.rs:52-56`
  (`SIGNABLE_BINARIES`); hardened runtime is conditional (`mod.rs:202-204`) because
  trusty-search/embedderd load an ONNX dylib at runtime.
- **No `.entitlements` file exists anywhere in the repo.** Notarization is manual and
  documented separately (`docs/reference/release-workflow.md:399-420`).

> **Harness implication:** a clean VM has **no Developer ID certificate**. The signed-install
> path therefore *cannot* be exercised end-to-end in a vanilla VM. Two options: (1) run the
> signed scripts in `--dry-run` mode to at least assert the command construction, or (2) treat
> signing as out of scope and assert that the *unsigned* `cargo install` path still yields
> executable, non-SIGKILLed binaries (the cdhash assertion, §7). **Track B recommends (2) as
> default and (1) as an opt-in `--check-signing` flag.**

### 2.7 Single-Install Convention — the binary table (verification source of truth)

Canonical doc: `docs/adr/0002-single-install-convention.md:30-37` — *"one crate that bundles
all of its required binaries via `[[bin]]` shims over a shared library, so a single
`cargo install --path crates/<name> --locked` puts the whole tool family (CLI + daemon +
sidecars) on PATH at one consistent version."* Refined by `docs/adr/0043-cargo-bin-policy.md:85`
(registry-only installs). Note there is **no tracked root `CLAUDE.md`** — it is guarded against
by `.github/workflows/claude-md-guard.yml` + `scripts/check_claude_md_not_tracked.sh`; the
workspace instruction file is `.trusty-mpm/INSTRUCTIONS.md`.

| Install command | Package | Version | Binaries that MUST appear on PATH | Cargo.toml lines |
|---|---|---|---|---|
| `cargo install trusty-search` | trusty-search | 0.40.0 | `trusty-search`, **`trusty-embedderd`** | 32-34, 36-38 |
| `cargo install trusty-memory` | trusty-memory | 0.22.0 | `trusty-memory`, **`trusty-bm25-daemon`**, **`trusty-memory-mcp-bridge`** | 23-38, 40-52, 54-70 |
| `cargo install trusty-mpm` | trusty-mpm | 1.3.0 | `trusty-mpm`, **`tm`** | 124-126, 129-131 |
| `cargo install trusty-analyze` | trusty-analyze | 0.8.0 | `trusty-analyze` | 23-25 |
| `cargo install trusty-review` | trusty-review | 0.11.0 | `trusty-review` | 16-19 |
| `cargo install trusty-console` | trusty-console | 0.5.0 | `trusty-console` | 33-35 |
| **`cargo install tga`** | tga (dir is `crates/trusty-git-analytics`) | 2.11.0 | `tga` | 26-28 |
| `cargo install trusty-installer` | trusty-installer | 0.5.0 | `trusty-installer`, **`tctl`** | 22-24, 29-31 |
| `cargo install trusty-code` | trusty-code | 0.3.0 | **`tcode`** (no `trusty-code` binary) | 16-18 |

Two traps encoded here:
1. `trusty-git-analytics` installs as **`cargo install tga`** — the package name is not the
   directory name.
2. `docs/reference/release-workflow.md:459` is **stale** — it claims `trusty-console` ships
   inside trusty-search/memory/analyze/review/mpm. The shim was removed from `trusty-memory`
   (see the comment at `crates/trusty-memory/Cargo.toml:72-75`, issue #1318, single-owner-per-binary).
   **Do not assert `trusty-console` appears from a `trusty-search`/`trusty-memory` install.**

`crates/trusty-embedderd` and `crates/trusty-bm25-daemon` are **lib-only**; their binaries are
produced by the parent crates. That is the Single-Install Convention in action and is what the
harness must assert.

Never-published (`publish = false`), so must NOT appear after any install:
`cto-assistant`, `tc-services`, `trusty-agents-local`, `trusty-code-gui`, `trusty-cto-db`,
`trusty-mpm-gui`, `trusty-publish-guard`.

---

## 3. Required toolchain inventory for the VM

### 3.1 What `macos-tahoe-base` already ships

From `cirruslabs/macos-image-templates` `templates/base.pkr.hcl` (fetched 2026-07-30):

| Present | Evidence |
|---|---|
| **Homebrew** (and therefore **Xcode Command Line Tools**, installed by brew's own installer) | `base.pkr.hcl:52` runs the official `Homebrew/install/HEAD/install.sh` |
| `~/.zprofile` with `brew shellenv`, `HOMEBREW_NO_AUTO_UPDATE=1`, `LANG=en_US.UTF-8` | `base.pkr.hcl:53-56` |
| **cmake, gcc, git-lfs, jq, yq, gh, wget, unzip, zip, ca-certificates, curl** | `base.pkr.hcl:66,69` |
| **mise** | `base.pkr.hcl:109` |
| **node@24**, rbenv/Ruby, awscli, buildkite-agent, otel-cli, git-credential-manager | `:120`, `:106-107`, `:136`, `:67-70` |
| **Rosetta** | `:72` |
| **tart-guest-agent** as a LaunchAgent (`--run-rpc` ⇒ `tart exec` + `tart ip --resolver=agent` work) and tart-guest-daemon (`--resize-disk`) | `:167`, `:175-177`; agent capabilities per `cirruslabs/tart-guest-agent` README |
| Spotlight disabled, `maxfiles` raised | `:33-38` |
| User `admin` / password `admin`, SSH (Remote Login) enabled | `base.pkr.hcl:19-20` (`ssh_username`/`ssh_password`) — Packer SSHes in, so Remote Login is on |
| VM shape: **4 vCPU / 8 GB / 50 GB** | `base.pkr.hcl:16-18`; matches local `~/.tart/vms/tahoe-base/config.json` (`cpuCount:4`, `memorySize:8589934592`) |

**Not present, must be added:** `rustup`/`cargo` (no Rust anywhere in the template), `uv`,
`pnpm`, `tmux`, `claude` (Claude Code), Developer ID certificate.

### 3.2 What this repo actually requires

| Requirement | Verdict | Evidence |
|---|---|---|
| **Rust ≥ 1.91** (edition 2024 ⇒ ≥1.85; aws-smithy indirect deps ⇒ 1.91) | **Mandatory** | `Cargo.toml:76-80`; CI pins `dtolnay/rust-toolchain@1.91` at `ci.yml:713`, `release.yml:864-866`, `version-parity.yml:39` |
| `rust-toolchain.toml` | **Only one, and not at root** — `crates/trusty-git-analytics/rust-toolchain.toml:1-3` (`channel = "stable"`, components rustfmt+clippy). The workspace root has none, so nothing auto-selects 1.91. **The harness must pin it explicitly.** | — |
| mise / asdf config in repo | **None.** Exhaustive search found no `.mise.toml`, `mise.toml`, `.tool-versions`, or `.config/mise/`. | — |
| C/C++ toolchain + `make` + `perl` | **Mandatory** — `usearch` (`cxx-build`), `tree-sitter` grammars, `tokenizers` (`onig_sys`, `esaxx-rs`), `libgit2-sys`, `openssl-src` all compile C/C++ from source | `Cargo.toml:157`, `:327`, `:339-346`, `:280`; `docs/trusty-agents/developer/building.md:6-8`. All ship with CLT / base macOS. |
| Xcode **full** | **Not needed** — CLT only | `docs/installation-tiers-draft.md:74-76`, `docs/runbooks/clean-vm-demo-rehearsal.md:17`, `10-isolation-testing-harness.md:254` |
| `protoc` / protobuf-compiler | **Not needed** — `protobuf = "3"` (`Cargo.toml:322`) is the pure-Rust runtime lib; no `prost-build`/`tonic-build`/`protobuf-src` in `Cargo.lock` | — |
| System OpenSSL / libgit2 / SQLite / zstd | **Not needed — all vendored/bundled**: `vendored-libgit2` + `vendored-openssl` (`Cargo.toml:280`), `rusqlite` `bundled` (`Cargo.toml:275-276`), `reqwest` uses `rustls-tls` only (`Cargo.toml:167`), no `zstd-sys` in the lock | — |
| `cmake` | Needed only for the optional `memory-core-kuzu` feature (`crates/trusty-common/Cargo.toml:170`, `:437`; not in `default = []` at `:266`). **Already in the base image anyway.** | — |
| **Outbound network during `cargo build`** | **Mandatory and non-obvious.** `ort-sys` *downloads* a prebuilt ONNX Runtime archive at build time, and `bundled-ort` is a **default feature** of trusty-search (`Cargo.toml:266`), trusty-analyze (`:135`), and trusty-embedderd (`:108`). | `Cargo.lock:6933-6941` (`ort-sys` deps on `ureq`); `crates/trusty-common/Cargo.toml:328` |
| Node 20 + pnpm | **Optional.** Five `build.rs` files shell out to pnpm/npm but all degrade gracefully with a `cargo:warning` + placeholder `index.html` (`crates/trusty-analyze/build.rs:72-79`, `crates/trusty-agents/build.rs:71-75`). CI sets **`SKIP_UI_BUILD=1` workspace-wide** (`ci.yml:90-91`). | — |
| `SKIP_UI_BUILD=1` | **The single most important build-time env var** for a Node-less VM. Read at `crates/trusty-{agents,analyze,console,memory,search}/build.rs`. | `ci.yml:35`, `:89-91` |
| `git` | Soft dep — `build.rs` emits `GIT_COMMIT_HASH`/`GIT_COMMIT_DATE`, falling back to `"unknown"` (`crates/trusty-agents/build.rs:163-179`, `crates/trusty-code/build.rs:103-104`) | — |
| Python / `uv` | **Not required to build or install.** No workflow references `setup-python`/`setup-uv`/`uv sync`. Only `python/trusty-voice/` (`pyproject.toml:6`, `>=3.12`) uses it, outside the Cargo graph and outside CI. DOC-10 wants `uv` for a claude-mpm orchestrator leg that is **not** in `stable_set.rs`. | — |
| `tmux`, Claude Code | **Runtime**, auto-installed by the installer on missing. `docs/runbooks/clean-vm-demo-rehearsal.md:38` calls this out as *"never hit on Bob's live machine, only here on fresh VMs"* — **so the VM must NOT prebake them; that auto-install path is exactly what we want to exercise.** | `docs/runbooks/mac-laptop-demo-runbook.md:44,486` |
| Secret/config env vars at compile time | **None.** No `option_env!` in any `build.rs`; only `env!("GIT_COMMIT_*")` which build.rs itself emits. | — |
| **`brew install` steps in CI** | **Zero, in any workflow.** The `macos-14` release leg relies entirely on the runner image. Consistent with the "vendor everything + CLT" story. | `.github/workflows/*` |

### 3.3 The mise tool set to bake into the golden image

```toml
# scripts/vmtest/golden/toolchain.mise.toml  (copied into the golden VM at ~/.config/mise/config.toml)
[tools]
rust = "1.91"      # Cargo.toml:79 rust-version; ci.yml:713 / release.yml:864 pin the same
uv   = "latest"    # not required by evidence; kept because the orchestrator leg (DOC-10 §6) wants it
gh   = "latest"    # base image has brew's gh; pinning via mise makes the golden reproducible
```

Deliberately **excluded** from the golden, with reasons:

| Excluded | Why |
|---|---|
| `node` / `pnpm` | `SKIP_UI_BUILD=1` is the default; opt-in via `--with-ui` (which adds `node@20` + `pnpm`). Note: with `SKIP_UI_BUILD=1` the built binaries serve *placeholder* UIs, so any web-UI assertion must be skipped in that mode. Pattern (a)-released does **not** need it — published crates ship a committed `ui-dist/` (`10-isolation-testing-harness.md:481-484`). |
| `tmux`, Claude Code | Must be absent so the installer's auto-install-on-missing path is exercised (§3.2). |
| Any trusty binary, `~/.cargo/registry` entries for trusty crates, `~/.trusty-*`, `.mcp.json`, launch agents | Would break the clean-VM guarantee. |
| Developer ID certificate | Would leak host credentials into a persistent image. Non-negotiable. |

**Correction to the flow given in the brief.** The example does
`echo 'eval "$(~/.local/bin/mise activate zsh)"' >> ~/.zshrc`. That is wrong for this harness:
`tart exec <vm> zsh -lc '...'` runs a **login, non-interactive** shell, which reads
`~/.zshenv`, `~/.zprofile`, `~/.zlogin` — but **not `~/.zshrc`** (interactive-only). Also,
`mise` is already installed by brew at `/opt/homebrew/bin/mise` (`base.pkr.hcl:109`), so
`curl https://mise.run | sh` is redundant. Use instead:

```sh
# put shims on PATH for EVERY zsh invocation (login or not)
printf 'export PATH="$HOME/.local/share/mise/shims:$PATH"\n' >> ~/.zshenv
# and activate for login shells (gives `mise exec`, env-var handling, etc.)
printf 'eval "$(/opt/homebrew/bin/mise activate zsh)"\n'      >> ~/.zprofile
```

---

## 4. Verdict: Cirrus CLI vs raw Tart

**Verdict: raw `tart`, shell-driven. Do not adopt Cirrus CLI for v1. Keep the door open.**

Verified facts about Cirrus's Tart integration:

| Capability | Reality | Impact |
|---|---|---|
| Ephemeral VM cloning | Yes — `cirrus run` with `macos_instance: image:` clones a local or remote VM, runs, and deletes it. | ✅ Matches the requirement… |
| …but **deletes on completion** | You get no shell into the failed VM. | ❌ **Decisive for an ad-hoc debugging tool.** With raw tart, a `--keep` flag lets you `tart exec -it <vm> zsh` into the exact failed state. |
| Host directory mount | **No `--dir`-equivalent for macOS guests.** The working dir is **rsynced** into the VM, honoring `.gitignore`. `--dirty` operates against the real working dir — documented for *containers* ("mount your project directory"), and it is precisely the mode that would let a guest build write to the host `target/`. | ⚠️ The rsync default is genuinely good (it would carry ~85 MB, not 29 GB). But you lose the ability to mount a *staging* dir, and `--dirty` is the mode you must never use here. |
| Parameterization | `-e NAME=VALUE` / `--env-file`. Scenario selection = task name. | ⚠️ Workable, but ad-hoc parametrized runs (which branch? which version pin? which crate subset? keep-or-delete?) become a wall of `-e` flags feeding a giant YAML `script:` block — i.e. the logic lives in shell regardless, just with a YAML wrapper tax. |
| Log/artifact capture on failure | `artifacts:` instruction + `--artifacts-dir` → `$PWD/artifacts/{Task}/{Instruction}/…`. Genuinely nice. | ✅ The one real win. Easily replicated with a run-dir + a rw egress mount (§5). |
| **macOS 15+/Tahoe "Local Network" permission** | Confirmed in `cirruslabs/cirrus-cli` README lines 7-24: requires accepting a **GUI pop-up on each host** running `cirrus run` with Tart VMs, or invoking `cirrus run` as **`root` with `--user <you>`** (spawning a privileged `cirrus localnetworkhelper`), or `sudo defaults write com.apple.network.local-network AllowedEthernetLocalNetworkAddresses …`. | ❌❌ **The killer.** This host is Darwin 25.5 (Tahoe). Requiring `sudo` or a GUI accept for a headless ad-hoc harness is unacceptable — and it is exactly the friction `tart exec` (vsock, no IP networking) avoids entirely. |
| Two-stage golden image build | Out of scope for Cirrus — that's a `tart`/Packer job either way. | — |
| Parallel task matrix | Real and useful — but for a *manually invoked* single-scenario harness on one laptop with a 50 GB-per-clone footprint, running 3 patterns in parallel is a disk/CPU liability, not a feature. | — |

**When to revisit.** Adopt Cirrus if and only if one of these becomes true:
(a) the harness graduates to a dedicated always-on build Mac where a parallel matrix pays for
itself; (b) you want the *same file* to run locally and on Cirrus Cloud; (c) the Local Network
permission is resolved by a one-time `sudo defaults write` you're willing to bless on the build
host. The architecture in §5 is deliberately layered so this is a cheap migration: the
per-pattern scenario scripts do **no VM lifecycle**, so a `.cirrus.yml` could later call them
verbatim as task bodies.

---

## 5. Recommended architecture

### 5.1 Directory layout

```
scripts/vmtest/                          # NOT under crates/ — Cargo.toml:15 members=["crates/*"]
├── vmtest                               # single executable entrypoint (bash, no extension)
├── README.md                            # how to run; what "clean" means; how to debug a failure
├── lib/
│   ├── common.sh                        # log/die/timing, run-dir mgmt, assert_* (copy tests/test-install-path.sh:63 shape)
│   ├── vm_macos_tart.sh                 # ⟵ THE OS BOUNDARY. clone/set/boot/exec/put/get/stop/delete
│   ├── provision.sh                     # mise-driven toolchain install (OS-agnostic body, called via vm_* verbs)
│   ├── source_xfer.sh                   # host→guest source snapshot (pattern c)
│   └── verify.sh                        # the assertion battery (§7)
├── golden/
│   ├── build-golden.sh                  # tahoe-base → trusty-toolchain-<tag>; run rarely, by hand
│   └── toolchain.mise.toml              # the exact pins baked in (§3.3)
├── scenarios/
│   ├── _scenario.sh                     # shared step-sequence runner (the upgrade-testing seam, §10)
│   ├── released-prebuilt.sh             # pattern a1 — install.sh → tctl (prebuilt tarballs; the real user path)
│   ├── released-cratesio.sh             # pattern a2 — cargo install <crate> --locked from crates.io
│   ├── branch.sh                        # pattern b  — clone branch, cargo install --path
│   ├── local.sh                         # pattern c  — host worktree snapshot, cargo install --path
│   └── no-toolchain.sh                  # negative: clone tahoe-base (NOT golden), assert guide-and-abort
├── fixtures/
│   └── sample-repo/                     # reuse docker/e2e/fixtures/sample-repo (symlink or copy); note the
│                                        # "must not be named fixtures/" walker caveat (docker/e2e/Dockerfile:83)
└── verify/
    └── expected-binaries.tsv            # the §2.7 table, machine-readable: <install-target>\t<bin>[,<bin>...]
```

Plus:
- `Makefile` additions: `vmtest-golden`, `vmtest-released`, `vmtest-branch`, `vmtest-local`,
  `vmtest-shell` — thin wrappers, matching the existing `e2e-docker` target style (`Makefile:64`).
- `.gitignore` addition: `/.vmtest-runs/`.

### 5.2 What each file does

| File | Responsibility |
|---|---|
| `vmtest` | Arg parsing (`--pattern`, `--from base\|golden`, `--version`, `--branch`, `--repo`, `--crates`, `--keep`, `--with-ui`, `--cpu`, `--memory`, `--disk`), run-dir creation, dispatch to a scenario, teardown via `trap`, final PASS/FAIL summary + exit code. |
| `lib/vm_macos_tart.sh` | **The only file that knows about `tart`.** Exports a stable verb set: `vm_create`, `vm_boot`, `vm_exec`, `vm_exec_stdin`, `vm_put`, `vm_get`, `vm_destroy`. Everything above this line is OS-agnostic. A future `lib/vm_linux_lima.sh` implements the same verbs (§10). |
| `lib/provision.sh` | mise activation + `mise use -g …` + a toolchain self-check. Runs against the *golden build* VM, not per-run (unless `--from base`). |
| `lib/source_xfer.sh` | Builds the host-side snapshot tarball and unpacks it in the guest (§6.4). |
| `lib/verify.sh` | Reads `verify/expected-binaries.tsv` and runs the §7 battery. Pure assertions; no VM knowledge beyond `vm_exec`. |
| `scenarios/*.sh` | **A scenario is an ordered list of steps.** Each step is `step <name> <shell-command>`. No VM lifecycle. This is the seam that makes upgrade testing a pure addition (§10). |

### 5.3 Run artifacts

```
.vmtest-runs/20260730T164500Z-local-a1b2c3/
├── manifest.json      # {pattern, base_image, golden_tag, git_sha, host, cpu, mem, disk, started, finished, verdict}
├── run.log            # everything, timestamped
├── steps/
│   ├── 01-clone.rc  01-clone.out  01-clone.err
│   ├── 02-boot.rc   …
│   └── 07-verify.rc …
└── guest/             # pulled out of the VM via the rw egress mount
    ├── stack-health.json
    ├── mcp.json
    ├── cargo-bin-listing.txt
    └── launchctl-print.txt
```

---

## 6. Concrete implementation sketch

All commands below are real and runnable. `tart` = `/opt/homebrew/bin/tart`.

### 6.1 The tart wrapper (`lib/vm_macos_tart.sh`)

```bash
# shellcheck shell=bash
TART="${TART:-/opt/homebrew/bin/tart}"

vm_create() {                      # vm_create <src> <name> <cpu> <mem_mb> <disk_gb>
  local src="$1" name="$2" cpu="$3" mem="$4" disk="$5"
  "$TART" clone "$src" "$name"                     # APFS CoW: ~1-3s regardless of the 50GB size
  "$TART" set "$name" --cpu "$cpu" --memory "$mem" --disk-size "$disk" --random-mac
}

vm_boot() {                        # vm_boot <name> [extra tart-run args...]
  local name="$1"; shift
  # --no-graphics: headless.  sync=none: this disk is disposable, so trade durability for I/O.
  # --net-softnet: userspace packet filter; isolates the VM from the host LAN.
  "$TART" run --no-graphics --root-disk-opts="sync=none" "$@" "$name" \
      >"$RUN_DIR/steps/vm-console.log" 2>&1 &
  echo $! > "$RUN_DIR/vm.pid"

  # Wait for the guest agent (NOT for an IP — we never need one).
  local i
  for i in $(seq 1 90); do
    if "$TART" exec "$name" /usr/bin/true >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  die "guest agent did not come up within 180s"
}

# Login-shell semantics so ~/.zshenv + ~/.zprofile (brew shellenv, mise shims) are loaded.
vm_exec() {                        # vm_exec <name> <script-string>
  local name="$1"; shift
  "$TART" exec "$name" /bin/zsh -lc "$*"
}

vm_exec_stdin() {                  # vm_exec_stdin <name> <script-string>   (host stdin → guest stdin)
  local name="$1"; shift
  "$TART" exec -i "$name" /bin/zsh -lc "$*"
}

vm_destroy() {                     # vm_destroy <name>
  local name="$1"
  "$TART" stop "$name"   >/dev/null 2>&1 || true
  sleep 2
  "$TART" delete "$name" >/dev/null 2>&1 || true
}
```

**Bootstrap self-test (run once at harness start — do not assume):**

```bash
vm_exec "$VM" 'exit 42'; [ $? -eq 42 ] || die "tart exec does not propagate exit codes; harness assertions are unsound"
```

This matters: every assertion in §7 depends on `tart exec` faithfully returning the guest
command's exit status. Verify it rather than trust it.

### 6.2 Golden image build (`golden/build-golden.sh`) — run by hand, rarely

```bash
#!/usr/bin/env bash
set -euo pipefail
TART=/opt/homebrew/bin/tart
BASE=tahoe-base
TAG="trusty-toolchain-$(date -u +%Y%m%d)"
BUILD="${TAG}-build"

"$TART" clone "$BASE" "$BUILD"
"$TART" set   "$BUILD" --cpu 8 --memory 16384 --disk-size 120
"$TART" run   --no-graphics "$BUILD" & sleep 45
until "$TART" exec "$BUILD" /usr/bin/true 2>/dev/null; do sleep 2; done

# ---- shell wiring: .zshenv is read by EVERY zsh; .zprofile by login shells (what `zsh -l` uses).
"$TART" exec "$BUILD" /bin/zsh -lc '
  set -euo pipefail
  command -v mise >/dev/null || brew install mise           # base.pkr.hcl:109 already installs it
  grep -q "mise/shims" ~/.zshenv 2>/dev/null || \
    printf "export PATH=\"\$HOME/.local/share/mise/shims:\$PATH\"\n" >> ~/.zshenv
  grep -q "mise activate" ~/.zprofile 2>/dev/null || \
    printf "eval \"\$(/opt/homebrew/bin/mise activate zsh)\"\n" >> ~/.zprofile
'

# ---- toolchain pins (§3.3)
"$TART" exec "$BUILD" /bin/zsh -lc '
  set -euo pipefail
  mkdir -p ~/.config/mise
  cat > ~/.config/mise/config.toml <<EOF
[tools]
rust = "1.91"
uv   = "latest"
gh   = "latest"
EOF
  mise install
  mise ls
'

# ---- self-check + purity check (fail the golden build if it is contaminated)
"$TART" exec "$BUILD" /bin/zsh -lc '
  set -euo pipefail
  xcode-select --print-path
  cc --version | head -1
  cargo --version && rustc --version
  gh --version | head -1
  uv --version
  test "$(rustc --version | cut -d" " -f2)" = "1.91.0" || echo "WARN: rustc is not exactly 1.91.0"
  # PURITY: the golden must contain zero trusty artifacts
  ! ls ~/.cargo/bin/trusty-* 2>/dev/null
  ! ls ~/.local/bin/trusty-* ~/.local/bin/tctl ~/.local/bin/tm 2>/dev/null
  ! test -d ~/.trusty-tools && ! test -d ~/.trusty-mpm
  ! ls ~/Library/LaunchAgents/com.trusty.* 2>/dev/null
  ! command -v tmux   >/dev/null   # must stay absent (§3.2)
  ! command -v claude >/dev/null   # must stay absent (§3.2)
  echo "GOLDEN PURITY OK"
'

"$TART" stop "$BUILD"; sleep 3
"$TART" rename "$BUILD" "$TAG"
echo "golden image ready: $TAG"
```

**Does prebaking violate "clean VM"? No — and the purity check above is the proof
obligation.** The golden contains a Rust toolchain and CLI tools; it contains **zero**
trusty-tools artifacts, zero registry entries for trusty crates, zero daemons, zero
`.mcp.json`, zero launch agents, and deliberately still lacks `tmux`/`claude` so the
auto-install path is exercised. What *is* lost by prebaking: the ability to test the
"cargo is absent → guide-and-abort exit 3" path (DOC-10 §5 A0,
`10-isolation-testing-harness.md:334-340`). That is why `scenarios/no-toolchain.sh` exists
and runs `--from base`.

### 6.3 Per-run lifecycle (in `vmtest`)

```bash
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-${PATTERN}-$(openssl rand -hex 3)"
RUN_DIR="${REPO_ROOT}/.vmtest-runs/${RUN_ID}"
mkdir -p "$RUN_DIR/steps" "$RUN_DIR/guest" "$RUN_DIR/stage"
VM="vmtest-${RUN_ID}"

cleanup() {
  local rc=$?
  if [ "${KEEP:-0}" = "1" ] && [ "$rc" -ne 0 ]; then
    warn "VM kept for debugging: $VM"
    warn "  shell in:  ${TART} exec -it ${VM} /bin/zsh -l"
    warn "  clean up:  ${TART} stop ${VM} && ${TART} delete ${VM}"
  else
    vm_destroy "$VM"
  fi
  return $rc
}
trap cleanup EXIT INT TERM

vm_create "${SRC_IMAGE}" "$VM" "${CPU:-8}" "${MEM:-16384}" "${DISK:-120}"
vm_boot   "$VM" \
  ${STAGE_MOUNT:+--dir="stage:${RUN_DIR}/stage:ro"} \
  --dir="out:${RUN_DIR}/guest"
```

Guest-side mount points (macOS auto-mounts virtiofs under `/Volumes/My Shared Files`):
`/Volumes/My Shared Files/stage` (read-only, source snapshot in) and
`/Volumes/My Shared Files/out` (read-write, artifacts out).

### 6.4 Pattern (c) — local host source: the transfer mechanism

**The problem, measured on this host:**

| Path | Size |
|---|---|
| `/Users/mac/workspace/trusty-tools` (whole tree) | **29 GB** |
| `.claude/` (git worktrees; gitignored) | **21 GB** |
| `target/` (gitignored) | **6.6 GB** |
| `.git/` | 210 MB (`size-pack: 188.99 MiB`) |
| **`git archive HEAD` (tracked worktree)** | **84,940,800 B ≈ 81 MiB, 5,306 files** |

**Options compared:**

| Mechanism | Speed | Isolation purity | Host `target/` leak | Uncommitted changes | Verdict |
|---|---|---|---|---|---|
| `tart run --dir=src:<repo>` **rw**, build in place | Fastest (no copy) | ✗ | **YES — fatal.** Guest `cargo build` writes host `target/`; worse, the UI `build.rs` files write a placeholder `index.html` into the source tree (`crates/trusty-analyze/build.rs:72-79`) even with `SKIP_UI_BUILD=1` | ✓ | ❌ **Reject** |
| `--dir=src:<repo>:ro`, build with `CARGO_TARGET_DIR` inside guest | Fast | ✓ | No | ✓ | ⚠️ Rejected: the `build.rs` placeholder writers would fail on a read-only tree, and virtiofs would lazily traverse 29 GB of gitignored junk |
| **`--dir=stage:<staging>:ro` + tarball built from `git ls-files`** | Fast (~81 MiB, local virtiofs, no network) | ✓ | No | ✓ (see below) | ✅ **Recommended** |
| `rsync -e ssh` over the VM's IP | Slower; needs SSH + IP + Local Network permission | ✓ | No | ✓ | Fallback only |
| `git bundle` / `git clone --local` | Fast, tiny | ✓ | No | ✗ — **loses uncommitted work**, which is usually the whole point of a "local" test | Use for pattern (b) only |

**Host side (`lib/source_xfer.sh`):**

```bash
# Tracked + untracked-but-not-ignored = the true working tree minus everything in .gitignore.
# This is exactly what a user's `cargo install --path` would see, and it excludes
# target/ (6.6G), .claude/ (21G), node_modules, .trusty-search/, .fastembed_cache/, etc.
build_source_snapshot() {                    # build_source_snapshot <repo> <out.tar>
  local repo="$1" out="$2"
  ( cd "$repo" && git ls-files -z --cached --others --exclude-standard ) \
    | ( cd "$repo" && tar --null -T - -cf "$out" )
  # Cargo.lock is tracked, so --locked works in the guest. .git/ is intentionally NOT included;
  # build.rs falls back to GIT_COMMIT_HASH="unknown" (crates/trusty-agents/build.rs:163-177).
  # Pass --with-git to additionally include a shallow bundle if commit-hash fidelity matters.
}
```

**Guest side:**

```bash
vm_exec "$VM" '
  set -euo pipefail
  mkdir -p ~/src
  tar -xf "/Volumes/My Shared Files/stage/src.tar" -C ~/src
  test -f ~/src/Cargo.lock && test -d ~/src/crates/trusty-search
  echo "source snapshot extracted: $(find ~/src -type f | wc -l) files"
'
```

### 6.5 The three install patterns (real commands)

Common guest preamble for every source-building pattern:

```bash
GUEST_ENV='
  export SKIP_UI_BUILD=1                     # ci.yml:90-91 — no Node needed; UIs are placeholders
  export CARGO_TARGET_DIR="$HOME/.vmtest-target"   # CRITICAL: share build artifacts across the N
                                                   # `cargo install` invocations. Without this each
                                                   # one gets its own temp target dir and rebuilds
                                                   # the entire dependency graph from scratch.
  export CARGO_TERM_COLOR=never
  export RUST_BACKTRACE=1
'
```

#### (a1) released — prebuilt tarballs via `install.sh` (the real user path)

```bash
vm_exec_stdin "$VM" 'cat > ~/.gh_token && chmod 600 ~/.gh_token' < <(gh auth token)

vm_exec "$VM" '
  set -euo pipefail
  export GITHUB_TOKEN="$(cat ~/.gh_token)"   # install.sh:186 — raises the releases-API rate limit
  '"${TRUSTY_VERSION:+export TRUSTY_VERSION=$TRUSTY_VERSION}"'
  curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh \
    | sh -s -- -y --force        # -y: install.sh:701 + the tctl non-TTY gate. --force: defeat the
                                 # idempotency skip at install.sh:743-752 so verification is real.
  rm -f ~/.gh_token
'
vm_exec "$VM" 'export PATH="$HOME/.local/bin:$PATH"; tctl install -y --json | tee "/Volumes/My Shared Files/out/install.json"'
vm_exec "$VM" 'export PATH="$HOME/.local/bin:$PATH"; mkdir -p ~/proj && cd ~/proj && git init -q . && tctl ensure --scope project --wait --yes'
vm_exec "$VM" 'export PATH="$HOME/.local/bin:$PATH"; tctl stack health --json | tee "/Volumes/My Shared Files/out/stack-health.json"'
```

#### (a2) released — compile from crates.io

```bash
vm_exec "$VM" "$GUEST_ENV"'
  set -euo pipefail
  for spec in trusty-search trusty-memory trusty-analyze trusty-review trusty-console tga trusty-mpm trusty-installer; do
    echo "=== cargo install $spec ==="
    cargo install "$spec" --locked            # add @X.Y.Z to pin
  done
'
```
Note `tga`, not `trusty-git-analytics` (§2.7 trap #1).

#### (b) branch — clone once, install by path

```bash
vm_exec "$VM" "$GUEST_ENV"'
  set -euo pipefail
  git clone --depth 1 --branch "'"$BRANCH"'" https://github.com/bobmatnyc/trusty-tools ~/src
  cd ~/src && git rev-parse HEAD
'
vm_exec "$VM" "$GUEST_ENV"'
  set -euo pipefail
  for d in trusty-search trusty-memory trusty-analyze trusty-review trusty-console trusty-git-analytics trusty-mpm trusty-installer; do
    echo "=== cargo install --path crates/$d ==="
    cargo install --path "$HOME/src/crates/$d" --locked
  done
'
```

Clone-then-`--path` is preferred over `cargo install --git … --branch …` because the latter
re-clones the whole 210 MB repo **once per crate**. Provide `--via cargo-git` as an alternate
to also exercise the literal documented form:

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --branch "$BRANCH" trusty-search --locked
```

`--depth 1` is safe here: nothing in the *install* path needs history, and
`crates/trusty-agents/build.rs:27` only reads `.git/HEAD`.

#### (c) local — host worktree snapshot

```bash
build_source_snapshot "$REPO_ROOT" "$RUN_DIR/stage/src.tar"   # host, §6.4
# ... vm_boot with --dir="stage:$RUN_DIR/stage:ro" ...
vm_exec "$VM" 'mkdir -p ~/src && tar -xf "/Volumes/My Shared Files/stage/src.tar" -C ~/src'
vm_exec "$VM" "$GUEST_ENV"'
  set -euo pipefail
  for d in '"$CRATE_DIRS"'; do cargo install --path "$HOME/src/crates/$d" --locked; done
'
```

> **Do NOT run `tctl install` in patterns (b) or (c).** Per `docs/adr/0043-cargo-bin-policy.md:85`
> and `crates/trusty-installer/src/commands/install.rs:1-40`, `tctl install` sources from
> prebuilt tarballs / crates.io only — it would *replace* the source-built binaries under test.
> Use `tctl ensure --scope project --wait --yes`, `tctl up`, and `tctl stack health --json` for
> wiring and verification; use `tctl install --dry-run` (safe in any context, `cli.rs:217-226`)
> if you want to inspect the plan.

### 6.6 SSH — debug fallback only

`tart exec` is the primary channel (vsock; no IP, no host key, no password, no macOS
Local Network permission). For interactive post-mortem, inject a key once the VM is up:

```bash
vm_exec_stdin "$VM" '
  mkdir -p ~/.ssh && chmod 700 ~/.ssh &&
  cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys
' < ~/.ssh/id_ed25519.pub

IP="$("$TART" ip --resolver=agent --wait 60 "$VM")"     # agent resolver: works even under softnet
ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR \
    -o ConnectTimeout=10 \
    -i ~/.ssh/id_ed25519 "admin@${IP}"
```

Host-key churn is handled by `UserKnownHostsFile=/dev/null` + `StrictHostKeyChecking=no`.
`sshpass` is **not** recommended: it isn't in the base image, requires a host tap install, and
puts the password on the command line. Key injection over `tart exec -i` is strictly better.

### 6.7 GH token handling

- The repo is **PUBLIC** (`gh repo view bobmatnyc/trusty-tools --json visibility` → `PUBLIC`),
  so a token is needed only for (i) GitHub releases-API rate limits in `install.sh`
  (`install.sh:186`, `:211-241`) and (ii) any future private branch.
- **Channel: stdin over `tart exec -i`.** Never a CLI argument (visible in the guest's `ps`),
  never an image layer, never a `--dir` mount that outlives the run.
- Recommended minimum: a **fine-grained PAT with public-repo read only**, or
  `gh auth token` piped from the host (short-lived, revocable).
- If `gh` itself is needed in the guest: `printf '%s' "$TOKEN" | tart exec -i "$VM" zsh -lc 'gh auth login --with-token'`.
- **Always `rm -f ~/.gh_token` in the guest at the end of the auth step**, and never build a
  golden image from a VM that has held a token.

---

## 7. Success / verification criteria

"Installation succeeded" is defined by this battery. A run is green iff **every** gate passes.

### 7.1 Universal gates (all patterns)

| # | Gate | Command | Pass condition |
|---|---|---|---|
| G1 | Install command exit code | the scenario's install step | `0` |
| G2 | **Every expected binary on PATH** | `command -v <bin>` for each row of `verify/expected-binaries.tsv` | all found |
| G3 | **Single-Install Convention** | `trusty-search` ⇒ `{trusty-search, trusty-embedderd}`; `trusty-memory` ⇒ `{trusty-memory, trusty-bm25-daemon, trusty-memory-mcp-bridge}`; `trusty-mpm` ⇒ `{trusty-mpm, tm}` | exact set present (generalizes `docker/e2e/smoke.sh:236-243`) |
| G4 | Correct location | `command -v` resolves under `~/.cargo/bin` (a2/b/c) or `~/.local/bin` (a1) | no stray PATH shadowing |
| G5 | `--version` works and is right | `<bin> --version` | exit 0; non-empty; equals the expected version (crates.io version for a1/a2, `Cargo.toml` version for b/c) |
| G6 | **cdhash safety** | exec each binary **twice** in succession; check for `Killed: 9` / exit 137 / signal 9 | never SIGKILLed. This is the regression gate for `.trusty-mpm/INSTRUCTIONS.md:412` and DOC-10 §3.4 (`:246-259`) |
| G7 | **Negative: no unpublishable binaries** | `ls ~/.cargo/bin ~/.local/bin` | contains none of `cto-assistant`, `tc-services`, `trusty-agents-local`, `trusty-code-gui`, `trusty-cto-db`, `trusty-mpm-gui`, `publish-guard` |
| G8 | Idempotency | rerun the install step | exit 0, reported as no-op / already-installed |
| G9 | No stderr panics | scan `steps/*.err` | no `panicked at`, no `error: linking`, no `SIGSEGV` |

### 7.2 Daemon / runtime gates (a1 primarily; a2/b/c where daemons were started)

Ports confirmed by observing the *host's* running stack (`lsof -nP -iTCP -sTCP:LISTEN`):

| Service | Port |
|---|---|
| trusty-memory | 7070 |
| trusty-console | 7788 |
| trusty-search | 7878 |
| trusty-analyze | 7879 |
| trusty-mpm | 7880 |
| trusty-review | 7891 |

| # | Gate | Command | Pass |
|---|---|---|---|
| G10 | Stack rollup | `tctl stack health --json` | `verdict == "ready"`, process exit == the JSON `exit_code`, `summary.down == 0` |
| G11 | Per-daemon HTTP | `curl -fsS http://127.0.0.1:<port>/health` for each of the six | HTTP 200 with a `status` field |
| G12 | **launchd registration** (the whole reason a VM is required) | `launchctl print gui/$(id -u)/com.trusty.memory` and `…/com.trusty.mpm.supervisor` | exit 0; state `running`. Labels confirmed from the host's `~/Library/LaunchAgents/` |
| G13 | **MCP registration** | read `~/proj/.mcp.json` | contains server entries for `trusty-search` (`serve`), `trusty-memory`/`trusty-review`/`trusty-mpm` (`serve --stdio`) per `crates/trusty-installer/src/commands/ensure/mcp_patch.rs:49-60` |
| G14 | Index reaches `fresh` non-racily | `tctl ensure --scope project --wait --yes` **then** `tctl stack health --json` | project cell `ready`. The `--wait` is what removes the flake (DOC-10 §5 A7, `:349-355`) |
| G15 | Functional smoke (port the docker battery) | index the sample repo, `trusty-search query` returns hits; memory palace→drawer→recall returns the sentinel; `tm start` then `tm status` reports running | as `docker/e2e/smoke.sh:100-120`, `:182-218`, `:266-280` |
| G16 | Auto-install-on-missing | `command -v tmux`, `command -v claude` **after** install | both now present (the golden deliberately lacked them — `docs/runbooks/clean-vm-demo-rehearsal.md:38-72`) |

### 7.3 Per-pattern deltas

| Pattern | Additional / omitted |
|---|---|
| **a1 prebuilt** | + assert binaries landed in `~/.local/bin` (`download/mod.rs:37`), + assert SHA-256 verification actually ran (`install.sh:526-538`, present in the log), + G16. Full G10–G15. |
| **a2 crates.io** | Full G1–G9 + G10–G15 after manually starting daemons or running `tctl up`. Versions must equal the **published** versions. |
| **b branch** | G1–G9 + G10–G15 via `tctl ensure`/`up` only (never `tctl install`, §2.3). Versions = the branch's `Cargo.toml` values. Record `git rev-parse HEAD` in `manifest.json`. |
| **c local** | Same as (b), plus: assert the snapshot file count matches the host's `git ls-files \| wc -l` ± untracked, and assert the guest built from `~/src` (not a stale registry copy) by checking `cargo install --path` appears in the log. |
| **no-toolchain (negative)** | Run `tctl …` on a **`tahoe-base`** clone with no cargo → assert **exit 3** and stderr contains the copy-paste `https://sh.rustup.rs` remediation (DOC-10 §5 A0, `10-isolation-testing-harness.md:334-340`). |

### 7.4 `verify/expected-binaries.tsv` (machine-readable §2.7 table)

```
# install-target	binaries (comma-separated)	source-crate-dir
trusty-search	trusty-search,trusty-embedderd	crates/trusty-search
trusty-memory	trusty-memory,trusty-bm25-daemon,trusty-memory-mcp-bridge	crates/trusty-memory
trusty-mpm	trusty-mpm,tm	crates/trusty-mpm
trusty-analyze	trusty-analyze	crates/trusty-analyze
trusty-review	trusty-review	crates/trusty-review
trusty-console	trusty-console	crates/trusty-console
tga	tga	crates/trusty-git-analytics
trusty-installer	trusty-installer,tctl	crates/trusty-installer
trusty-code	tcode	crates/trusty-code
```

This file is the single source of truth for G2/G3 and should be diffed against the `[[bin]]`
declarations by a tiny `--check-table` mode so it can't silently rot.

---

## 8. Isolation analysis

### 8.1 Host touchpoints, enumerated and measured

Measured on this host, 2026-07-30:

| Host touchpoint | Actual current state | Covered by the VM boundary? |
|---|---|---|
| `~/.cargo/bin` | 26 entries incl. `tctl`, `tga`, `tm`, `trusty-analyze`, `trusty-bm25-daemon`, `trusty-console`, `trusty-embedderd`, `trusty-embedderd-py`, `trusty-installer`, `trusty-memory`, `trusty-memory-mcp-bridge`, `trusty-mpm`, `trusty-review`, `trusty-search` | ✅ guest has its own `$HOME`, uid, and `~/.cargo` |
| `~/.cargo/registry`, `~/.cargo/git` | host build cache | ✅ never mounted |
| `~/.local/bin` | `claude`, `claude-mpm*`, `mcp-*`, `mise`, `tm`, `trusty-bm25-daemon`, `trusty-memory`, `trusty-mpm` | ✅ |
| `~/Library/LaunchAgents` | `com.trusty.memory.plist`, `com.trusty.mpm.supervisor.plist` | ✅ **This is the one a clean `$HOME` would NOT cover** — launchd keys on uid, not `$HOME` (DOC-10 §3.1, `:170-192`). The VM has a different uid *and* a different kernel. |
| **Running daemons / bound ports** | `127.0.0.1:7070` (memory), `:7788` (console), `:7878` (search), `:7879` (analyze), `:7880` (mpm), `:7891` (review) — 6 live processes | ✅ Guest binds inside its own network namespace. `--net-softnet` further restricts the VM (and blocks it from reaching the host's loopback, which is unreachable from a VM anyway). |
| `~/.trusty-mpm/`, `~/.trusty-tools/` | present (config, logs, supervisor plist target — `plist_bootstrap.rs:246,261-263,478`) | ✅ |
| `~/.claude/` (MCP config, agents, 21 GB of worktrees) | present | ✅ never mounted; excluded from the source snapshot by `.gitignore` (`.claude/` and `.claude/worktrees/`) |
| Project `.mcp.json` | present at repo root, and **gitignored** (`.gitignore` line `.mcp.json`) | ✅ so it is not even in the source snapshot; the guest's `tctl ensure` patches the guest's own copy |
| Keychain / Developer ID cert | host has signing identities | ✅ never injected; signing is out of scope by default (§2.6) |
| **Host `target/` (6.6 GB)** | present | ✅ **by construction** — the source snapshot is built from `git ls-files --cached --others --exclude-standard`, and `/target` + `**/target/` are the first two `.gitignore` entries, so `target/` is never transferred and the guest's `CARGO_TARGET_DIR` is inside the guest |
| Host `.fastembed_cache/`, `.trusty-search/`, `*.redb` | present, gitignored | ✅ excluded by the same mechanism |

### 8.2 The read-write-mount trap, addressed explicitly

The brief flags this correctly and it is the single most dangerous design choice available:

> *"if you mount the host repo dir read-write into the VM, `cargo build` inside the VM will
> write to the host `target/` — that IS a host effect."*

It is worse than that here. Even with `CARGO_TARGET_DIR` redirected inside the guest, five
`build.rs` files write a placeholder `index.html` **into the source tree** when pnpm is absent
or `SKIP_UI_BUILD=1` (`crates/trusty-analyze/build.rs:72-79`, `crates/trusty-agents/build.rs:71-75`,
and the console/memory/search equivalents), and `crates/trusty-search/build.rs:64-66` invokes
`make release-prep` **in the crate directory**. A read-write mount therefore mutates the host
worktree regardless of target-dir redirection. A read-only mount of the repo would make those
same writes *fail the build*.

**Resolution:** the repo directory is never mounted at all. Only a harness-owned staging dir is
mounted, read-only, containing a tarball. The guest unpacks into its own `$HOME/src`.

### 8.3 The one intentional host-write channel

`--dir="out:${RUN_DIR}/guest"` is mounted **read-write** so the guest can deposit
`stack-health.json`, `.mcp.json`, `launchctl print` output, and logs. This writes only into
`.vmtest-runs/<run-id>/guest/` under the repo, which is added to `.gitignore`. It is the sole
guest→host write path and it cannot reach `target/`, `~/.cargo`, or any trusty state.

### 8.4 Residual risks (honest)

- **`tart exec` runs in a launchd *agent* context**, not a full interactive login session. The
  guest agent README notes fewer privileges are available when run as a global agent. The base
  image enables auto-login (so a real GUI session for uid 501 exists and `launchctl … gui/501`
  works), but TCC-scoped behaviours (Full Disk Access, App-Data prompts — `.trusty-mpm/INSTRUCTIONS.md:414-418`)
  will differ from a human's Terminal session. **Consequence: the harness can prove
  "installs and runs", but cannot fully prove "TCC grants behave as a human user would see".**
  That gap must be stated in the README, not papered over.
- **Disk headroom.** The base image is a 50 GB raw disk with ~31 GB used → ~15–18 GB free in
  the guest. A release build of this workspace can plausibly exceed that. Mitigation:
  `tart set <clone> --disk-size 120` before boot (tart can only *grow* a disk). This relies on
  tart-guest-daemon's `--resize-disk`, which per its README requires the recovery partition to
  have been removed. **Verify on the first golden build; fall back to
  `diskutil apfs resizeContainer disk0s2 0` in-guest if the auto-resize does not fire.**
- **Concurrent runs.** CoW clones start near-zero but a full build materializes many GB each.
  With 538 GB free, cap concurrency at 2–3 and have `vmtest` refuse to start below a free-space
  floor.

---

## 9. Time / cost estimates and mitigations

### 9.1 Estimates

Host: Apple M5 Pro, 18 cores, 64 GB RAM, 538 GB free. Guest recommended: 8 vCPU / 16 GB / 120 GB.

| Phase | Estimate | Basis |
|---|---|---|
| `tart clone` (APFS CoW) | **1–3 s** | CoW; `tart help clone` — *"a cloned VM won't actually claim all the space right away"* |
| `tart set` (cpu/mem/disk) | < 1 s | — |
| Boot → guest agent responsive | **25–60 s** | macOS VM cold boot |
| Golden provisioning (**one-time**) | **5–10 min** | mise + rustup 1.91 (~400 MB toolchain) + uv + gh |
| Per-run toolchain (with golden) | **0 s** | prebaked |
| Per-run toolchain (`--from base`) | **3–8 min** | the saving the golden buys |
| **(a1) prebuilt via `install.sh`** | **4–8 min total** | tarball downloads + SHA verify + launchd bootstrap + `ensure --wait` + tmux/claude brew installs. **No compilation at all.** |
| **(a2) crates.io compile, 4-crate set** | **12–25 min** | extrapolated from `e2e-docker.yml:44-47` ("~15–30 min on a free GHA runner, 2 vCPU") scaled to 8 Apple-Silicon vCPU, minus Docker layer caching |
| **(a2) full 8-target stable set** | **20–40 min** | +4 smaller crates sharing the dep graph |
| **(b)/(c) source build, 4-crate subset** | **15–30 min** | same compile cost + 1–3 min transfer/clone; slightly higher because per-crate feature resolution differs from the crates.io path |
| **(b)/(c) full set** | **25–50 min** | — |
| Verification battery | **1–4 min** | daemon starts + `--wait` index + HTTP probes |
| Teardown (`stop` + `delete`) | **5–20 s** | — |
| **Total per run** | **a1 ≈ 5–10 min; a2/b/c ≈ 20–55 min** | |

**These are estimates, not measurements.** The first golden build and the first (a2) run should
be timed and the numbers written back into `scripts/vmtest/README.md`.

### 9.2 Is a from-source install of the whole stack feasible in a VM? Yes — with mitigations

| Mitigation | Effect | Cost to purity |
|---|---|---|
| **`CARGO_TARGET_DIR=$HOME/.vmtest-target`** shared across all `cargo install` calls | **The single biggest win.** Without it, `cargo install` uses a fresh temp target dir per invocation and rebuilds the *entire* dependency graph 8 times. Plausibly a 3–5× reduction on a multi-crate run. | **None** — the dir is inside the guest |
| `--locked` everywhere | Skips resolution; guarantees the tested lockfile. Already mandated by every repo script. | None (it *increases* fidelity) |
| `SKIP_UI_BUILD=1` (`ci.yml:90-91`) | Removes the pnpm/Vite pipeline and the Node dependency entirely | ⚠️ UIs become placeholders — **web-UI assertions must be skipped**; irrelevant for pattern (a1) since published crates ship `ui-dist/` |
| **Default to a crate subset** (`--crates search,memory,mpm,analyze`, matching `docker/e2e/Dockerfile:51-77`) with `--crates all` opt-in | Halves the common-case run | None |
| `tart run --root-disk-opts="sync=none"` | Disables disk sync on a disposable VM — meaningful I/O win for a link-heavy build | None (VM is thrown away) |
| `tart set --cpu 8 --memory 16384` (base default is 4/8 GB) | ~1.7–2× on a parallel cargo build | None |
| **`--warm` mode**: a *second-tier* image `trusty-warm-<sha>` with `~/.cargo/registry` + a populated `CARGO_TARGET_DIR` for a pinned commit | Turns a 25-min run into a 3–6-min incremental one | ❌ **Explicitly NOT a clean run.** Must be flagged loudly in `manifest.json` (`"clean": false`) and forbidden for any release-gating verdict. Iteration only. |
| `sccache` with a host-mounted rw cache dir | Cross-run object reuse | ❌ Leaks host state across runs; adds a variable. **Recommend against for v1.** |
| Prebake `~/.cargo/registry` in the *golden* | Saves 1–3 min of crate downloads | ⚠️ Gray zone: contains only third-party crates, no trusty artifacts — but it stops exercising the download path and drifts as deps change. **Recommend against**; the download is not the bottleneck. |
| Release vs debug | `cargo install` is always release. Don't fight it — a debug install would not be the thing under test. | — |

**Headline:** pattern (a1) is cheap enough to run casually (~5–10 min). Patterns (a2)/(b)/(c)
are 20–55 min and belong in the "kick it off, go get coffee, check the run dir" category —
which is exactly what "ad-hoc, manually run" implies. That is acceptable. The mitigation that
must not be skipped is the shared `CARGO_TARGET_DIR`.

---

## 10. Extension points

### 10.1 Upgrade testing (out of scope now, must not be precluded)

The design admits it as a **pure addition** because a scenario is already an ordered list of
steps rather than a monolith:

```bash
# scenarios/_scenario.sh — the seam
step() {                          # step <name> <command...>
  local name="$1"; shift
  local n; n="$(printf '%02d' "$((++STEP_N))")"
  log "STEP ${n} ${name}"
  ( "$@" ) >"$RUN_DIR/steps/${n}-${name}.out" 2>"$RUN_DIR/steps/${n}-${name}.err"
  local rc=$?; echo "$rc" > "$RUN_DIR/steps/${n}-${name}.rc"
  [ "$rc" -eq 0 ] || die "step ${name} failed (rc=$rc)"
}
```

An upgrade scenario is then literally:

```bash
# scenarios/upgrade.sh  (FUTURE — not built now)
step install-N-1  install_released "$FROM_VERSION"
step verify-N-1   verify_stack     "$FROM_VERSION"
step updates      vm_exec "$VM" 'tctl updates --json'
step upgrade      vm_exec "$VM" 'tctl upgrade --yes --json'
step verify-N     verify_stack     "$TO_VERSION"     # same battery, different expected versions
```

Three things make this work and must be preserved: (1) `verify.sh` takes the *expected version
set* as a parameter rather than hardcoding it; (2) `expected-binaries.tsv` is data, not code;
(3) no scenario performs VM lifecycle, so multi-phase scenarios reuse one VM. `tctl upgrade`,
`tctl updates`, and the verify-after contract already exist (`cli.rs:245-271`) — the harness
just needs to sequence them.

### 10.2 Linux later

The OS boundary is exactly one file: `lib/vm_macos_tart.sh`, exporting
`vm_create / vm_boot / vm_exec / vm_exec_stdin / vm_put / vm_get / vm_destroy`. A future
`lib/vm_linux_lima.sh` (or `vm_linux_docker.sh`) implements the same seven verbs; `vmtest`
selects via `--os`. Everything above the boundary — scenarios, verification, source transfer,
run dirs — is already OS-agnostic *except*:

| macOS-specific bit | Where it must be guarded |
|---|---|
| `launchctl print gui/$(id -u)/…` (G12) | `verify.sh`, behind `[ "$VM_OS" = macos ]`. Linux uses `systemctl --user` (per-PR-gated per DOC-10 §4.1, `:294-316`) or foreground supervision |
| cdhash/SIGKILL assertion (G6) | macOS-only; harmless no-op on Linux |
| Codesigning checks | macOS-only |
| `/Volumes/My Shared Files/…` mount paths | `vm_*.sh` should export `GUEST_STAGE_DIR` / `GUEST_OUT_DIR` rather than letting scenarios hardcode paths |
| Homebrew / `.zprofile` wiring | `provision.sh`, behind the same guard; Linux uses apt `build-essential pkg-config libssl-dev` (exactly `ci.yml:168-172`) |

Note that Linux already has partial coverage via `scripts/e2e-docker.sh` for the crates.io
pattern; the VM harness's Linux leg would add the **branch** and **local** patterns, which
Docker cannot do today (`docker/e2e/Dockerfile` copies no source at all).

### 10.3 CI later

Deliberately *not* wired to CI now. If it graduates: `.github/workflows/isolation.yml` running
the scenarios directly on a `macos-14` runner (which *is* a fresh isolated host — DOC-10 §3.2,
`:216-223`), reusing `scenarios/*.sh` verbatim because they contain no VM lifecycle.

---

## 11. Open questions and risks

| # | Item | Severity | Mitigation / next step |
|---|---|---|---|
| R1 | **Does `tart exec` propagate guest exit codes?** Every assertion depends on it. | **High** | The §6.1 bootstrap self-test (`exit 42`) — run it first, hard-fail the harness if it doesn't hold. |
| R2 | **Does `tart set --disk-size 120` actually grow the guest APFS container?** Auto-resize requires the recovery partition to be removed. | High | Verify on the first golden build; fallback `diskutil apfs resizeContainer disk0s2 0`. If neither works, the 15–18 GB of free space may not survive a full-workspace release build. |
| R3 | **All timing figures in §9 are estimates**, extrapolated from a 2-vCPU Linux GHA number. | Medium | Time the first golden build and the first (a2) run; write the measured numbers into `scripts/vmtest/README.md`. |
| R4 | **`tart exec` runs in a launchd agent context**, so TCC/FDA behaviour differs from a human session. | Medium | Document the limitation; do not claim TCC coverage. Consider a manual, GUI-attached (`tart run` without `--no-graphics`) confirmation run before a release. |
| R5 | **Signing cannot be tested in a clean VM** (no Developer ID cert), yet `tctl install` signs automatically (fail-soft) and the signed-install scripts hard-fail without a cert (`install-trusty-mpm-signed.sh:246-250`). | Medium | Default: skip. Opt-in `--check-signing` runs the scripts with `TRUSTY_CODESIGN_DRY_RUN=1` to at least assert command construction. Never inject a real identity into a VM. |
| R6 | **`SKIP_UI_BUILD=1` means placeholder UIs** in patterns b/c, so trusty-console's web UI cannot be meaningfully asserted there. | Medium | Either accept (assert HTTP 200 only, not content), or gate a `--with-ui` run that adds `node@20` + `pnpm` and costs ~5–15 min more. Pattern (a) is unaffected (published crates ship `ui-dist/`). |
| R7 | **`ort-sys` downloads ONNX Runtime at build time** — a network blip mid-build fails the run in a way that looks like a code failure. | Medium | Detect the signature in `steps/*.err` and classify it as INFRA, not FAIL. Consider a pre-build connectivity probe to the ORT download host. |
| R8 | **The `.mcp.json` assertion (G13) may be brittle** — the file is gitignored on the host, so its guest shape is only known from `mcp_patch.rs:49-60`. | Low | Assert *presence of the four server keys*, not exact JSON equality. |
| R9 | **`docs/reference/release-workflow.md:459` is stale** about `trusty-console` shims (§2.7 trap #2). A naive reading would produce a false assertion. | Low | Use `verify/expected-binaries.tsv` derived from actual `[[bin]]` declarations, and add a `--check-table` mode that diffs it against the Cargo.toml files. Separately: fix the stale doc. |
| R10 | **Divergence from DOC-10 §7.3** (which puts assertions in a Rust `#[ignore]` test). | Low | Deliberate, justified in §2.1. Should be reconciled with the DOC-10 owner — the doc is Accepted, and the crate it names (`trusty-controller`) no longer exists, so DOC-10 §7.3 needs an amendment regardless. |
| R11 | **Concurrency / disk exhaustion** if multiple runs overlap. | Low | `vmtest` refuses to start below a configurable free-space floor (default 150 GB) and warns above 2 concurrent VMs. |
| R12 | **Golden image drift.** Homebrew in the base image is a moving target; `HOMEBREW_NO_AUTO_UPDATE=1` helps but the base OCI tag itself moves. | Low | Pin the base by digest (`ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c830…`, already in the local cache) rather than `:latest`, and record base digest + golden tag in `manifest.json`. |
| Q1 | Should the *default* crate set be the 4 the Docker E2E covers, or the full `stable_set.rs:173-182` seven? | — | Recommend: default = the 7-member stable set + `trusty-installer` for pattern (a1) (it's cheap, no compiling); default = the 4-crate subset for the compiling patterns, `--crates all` to widen. |
| Q2 | Should `uv` be in the golden at all, given no evidence the stack needs it? | — | Keep it (cheap, requested, and DOC-10 §6 wants it for a future orchestrator leg), but document that it is unproven-necessary. |

---

## Appendix A — verified environment facts

| Fact | Command | Result |
|---|---|---|
| tart subcommands include `exec`, `clone`, `set`, `ip`, `stop`, `delete`, `rename`, `suspend` | `tart --help` | confirmed |
| `tart exec` requires the guest agent; `-i` attaches stdin, `-t` allocates a PTY | `tart help exec` | confirmed |
| `tart ip --resolver=agent` works without DHCP/ARP | `tart help ip` | confirmed |
| `tart run --dir="[name:]path[:ro][,tag=]"`; macOS guests auto-mount at `/Volumes/My Shared Files` | `tart help run` | confirmed |
| `tart run --root-disk-opts="sync=none"`, `--net-softnet`, `--no-graphics` | `tart help run` | confirmed |
| `tart set --cpu/--memory/--disk-size` (grow-only) | `tart help set` | confirmed |
| `tahoe-base` exists locally, 50 GB, stopped; OCI source digest `sha256:a8e1c830…` | `tart list` | confirmed |
| VM shape 4 vCPU / 8 GB / 50 GB raw | `~/.tart/vms/tahoe-base/config.json` | confirmed |
| Host: Apple M5 Pro, 18 cores, 64 GB RAM, 538 GB free | `sysctl`, `df -h` | confirmed |
| Repo tree 29 GB; `.claude/` 21 GB; `target/` 6.6 GB; `.git` 210 MB | `du -sh` | confirmed |
| `git archive HEAD` = 84,940,800 B; 5,306 tracked files | `git archive \| wc -c`, `git ls-files \| wc -l` | confirmed |
| Repo is PUBLIC, default branch `main` | `gh repo view` | confirmed |
| Host has 6 trusty daemons listening on 7070/7788/7878/7879/7880/7891 | `lsof -nP -iTCP -sTCP:LISTEN` | confirmed |
| Host has `com.trusty.memory.plist`, `com.trusty.mpm.supervisor.plist` LaunchAgents | `ls ~/Library/LaunchAgents` | confirmed |
| No `.cirrus.yml` in the repo; `cirrus` CLI supports `run`/`validate`/`worker` with `--tart-lazy-pull`, `--dirty`, `--artifacts-dir`, `--user` | `ls`, `cirrus run --help` | confirmed |
| Cirrus CLI README documents the macOS 15+ Local Network permission requirement and the `root --user` workaround | `gh api repos/cirruslabs/cirrus-cli/contents/README.md` lines 7-24 | confirmed |
| `tahoe-base` ships brew (⇒ Xcode CLT), cmake, gcc, gh, jq, yq, node@24, **mise**, rosetta, tart-guest-agent; user `admin`/`admin`; 4 CPU / 8 GB / 50 GB | `gh api …/macos-image-templates/contents/templates/base.pkr.hcl` (lines 16-20, 52-72, 106-136, 160-177) | confirmed |
| `scripts/check_line_cap.sh` caps tracked **`.rs`** files only (`git ls-files '*.rs'`, line 160) — shell scripts are exempt | file read | confirmed |
| `.pre-commit-config.yaml` has no shellcheck/shfmt hook | grep | confirmed |

## Appendix B — sources

- [cirruslabs/cirrus-cli](https://github.com/cirruslabs/cirrus-cli) — Local Network permission caveat, `--dirty` semantics, rsync-with-`.gitignore` default
- [Tart docs — Cirrus CLI integration](https://tart.run/integrations/cirrus-cli/) — `macos_instance` + local VM images, artifacts
- [cirruslabs/macos-image-templates](https://github.com/cirruslabs/macos-image-templates) — `templates/base.pkr.hcl`
- [cirruslabs/tart-guest-agent](https://github.com/cirruslabs/tart-guest-agent) — `--run-rpc` (`tart exec`), `--resize-disk`
