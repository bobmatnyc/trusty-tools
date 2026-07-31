# VM Install-Testing Harness for trusty-tools — Track A Research (Fable)

Date: 2026-07-30
Scope: design/recommendation only — no code changes, no VMs started.
Environment verified on this host: `tart 2.32.1`, `cirrus 1.0.0`, local VM `tahoe-base`
(50 GB, stopped, from `ghcr.io/cirruslabs/macos-tahoe-base:latest`), `mise` at
`~/.local/bin/mise`, `gh` authenticated.

---

## 1. Executive summary + headline recommendation

- **Script-driven with raw `tart`, not Cirrus CLI.** `cirrus run` solves problems we do
  not have (task-graph CI semantics) while taking away things we need (post-mortem access
  to a failed VM, parametrized ad-hoc runs, control of golden-image baking). Verdict
  detail in §4.
- **Bash harness at `scripts/vm-install/`**, deliberately outside the Cargo workspace and
  outside `cargo test`/CI — mirroring the existing `scripts/e2e-docker.sh` +
  `docker/e2e/smoke.sh` precedent. Not a Rust crate: anything under `crates/*` is
  auto-globbed into the workspace (`Cargo.toml:16`) and would join the `make check`
  quality gates, which contradicts the "separate from test infrastructure" requirement.
- **Two-stage image strategy**: `tahoe-base` → one-time golden bake
  `tahoe-trusty-toolchain` (mise-activated rust@stable, tmux, warmed cargo registry —
  **zero trusty binaries or state**) → per-run ephemeral APFS-CoW clone, deleted after
  the run. Saves ~8–12 min and one class of network flake per run; does not violate
  "clean VM" because cleanliness is defined over the system under test (trusty
  artifacts), not over generic toolchain.
- **Reuse, don't reinvent, the repo's own install machinery**: pattern (a) *released*
  should primarily exercise the real user path — `install.sh` → `trusty-installer
  install -y` (prebuilt-first, sha256-verified, ends with its own `ensure` + `stack
  health` verification) — with `cargo install <crate> --locked` as a secondary variant
  mirroring `docker/e2e/Dockerfile`. Verification logic should be ported from
  `docker/e2e/smoke.sh`, which already encodes daemon health, indexing, and query smoke
  checks.
- **SSH via key injection over `tart exec`** (guest agent is preinstalled in the base
  image — no sshpass, no interactive password), host source transfer for pattern (c) via
  a **git-aware tar stream over ssh** (`git ls-files -co --exclude-standard | tar | ssh`)
  — never a read-write virtiofs mount, which would let the VM's `cargo build` write into
  the host `target/`.

---

## 2. Findings: existing install machinery (with citations)

### 2.1 `install.sh` (repo root, 785 lines)
The canonical end-user entry point (`curl … | sh`):
- Downloads a **prebuilt** `trusty-installer` release tarball from GitHub Releases,
  SHA-256-verifies it, installs `trusty-installer` **and the `tctl` alias** into
  `~/.local/bin` (`install.sh:43-52`, `505-571`).
- Honors `TRUSTY_YES=1` / `-y` for non-interactive runs, `TRUSTY_VERSION` for pinning,
  and `GITHUB_TOKEN`/`GH_TOKEN` purely to raise the GitHub API rate limit from 60/hr to
  5000/hr — never required (`install.sh:21-31`, `186-192`).
- Then runs `trusty-installer install -y`, **propagating its exit code**
  (`install.sh:592-621`, `766-772`). The success box explicitly states installation is
  "complete **and verified** (ensure + stack health passed)" (`install.sh:630-643`) —
  i.e. the installer already contains its own verification tail
  (`crates/trusty-installer/src/commands/verify_tail.rs`).

### 2.2 `crates/trusty-installer` (the control plane)
- **Stable set** — the coherent platform installed/upgraded as a unit
  (`src/commands/stable_set.rs:173-183`): `trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-review`, `tga`, `trusty-console`, `trusty-mpm`. Required vs
  optional and daemon-vs-CLI flags are data on each member; a *required* member failing
  install exits 2 / "NOT VERIFIED" (`stable_set.rs:66-72`).
- **Prebuilt-first install strategy** (`src/commands/install.rs:6-14`): on a Tier-1
  platform a prebuilt GitHub-Releases tarball is downloaded (`src/download/`), with
  fallback to `cargo install <crate> --locked`. `aarch64-apple-darwin` is a released
  target (`.github/workflows/release.yml:442`, `575-578`, `1024`) — so **inside the VM
  the released pattern needs no Rust compile at all** on the happy path.
- **External prereqs table** (`src/commands/prereqs/mod.rs:84-110`): `claude` (for
  trusty-mpm), `git` (all members), `tmux` (trusty-mpm). Missing prereqs warn (with
  auto-install attempts via detected package manager, `prereqs/phase.rs`).
- **MCP registration** (`src/commands/ensure/mcp_patch.rs:23-30`): `tctl ensure` patches
  the **project-local `.mcp.json`** (cwd) with `mcpServers` entries for search, memory,
  review, mpm — idempotent upsert. This gives us a concrete, assertable artifact for
  "MCP server registration" verification.
- Lifecycle: shared daemons are launchd-managed (`~/Library/LaunchAgents` plists,
  `plist_bootstrap.rs`); trusty-mpm is self-managed via its own `start|stop|restart`
  verbs (`stable_set.rs:27-41`).

### 2.3 `Makefile`
- `check` (`Makefile:46-49`) — the cargo test/clippy/fmt gate; the harness must stay out
  of it.
- `e2e-docker` (`Makefile:64-69`) — delegates to `scripts/e2e-docker.sh` with per-tool
  version pin variables.
- `install-search-signed` / `install-mpm-signed` (`Makefile:94-121`) — Developer-ID
  codesigning wrappers fixing macOS TCC cdhash churn (#873/#2721). **Not applicable in
  the VM** (no Developer ID cert in the guest; TCC grants inside an ephemeral VM are
  irrelevant), but they document env overrides worth mirroring:
  `TRUSTY_INSTALL_PATH`, `TRUSTY_INSTALL_VERSION`
  (`scripts/install-trusty-mpm-signed.sh:1-70`).

### 2.4 Docker E2E (the closest existing analogue)
- `scripts/e2e-docker.sh` builds `docker/e2e/Dockerfile` and runs `smoke.sh`, streaming
  logs to a bind-mounted host dir (`e2e-docker.sh:137-147`).
- `docker/e2e/Dockerfile` installs `trusty-search`, `trusty-memory`, `trusty-mpm`,
  `trusty-analyze` via `cargo install <crate> [--version X] --locked` on `rust:slim` with
  `pkg-config libssl-dev curl procps jq build-essential cmake` (Dockerfile lines 20-35,
  50-77). Env conventions to reuse: `TRUSTY_SKIP_RAM_CHECK=1` (bypasses a 16 GB RAM
  guard), `XDG_DATA_HOME` sandboxing of daemon state, `E2E_LOG_DIR`.
- `docker/e2e/smoke.sh` is a **ready-made verification vocabulary**: `wait_http` health
  polling, `trusty-search start` + `trusty-search port` + `/health`, lexical-only
  indexing of a fixture (avoiding ONNX), query assertion (`smoke.sh:40-120`). Fixture
  gotcha documented there: no path component named `fixtures` (trusty-search SKIP_DIRS).
- `.github/workflows/e2e-docker.yml:51-65`: nightly cron + `workflow_dispatch` with
  version inputs — confirming the project's pattern of *manual/scheduled* install gates
  outside PR CI (`docs/reference/release-workflow.md:440-450`).

### 2.5 Single-Install Convention (what "all binaries present" means)
`docs/reference/release-workflow.md:451-463` (epic #943) plus crate manifests:

| `cargo install` of | Must produce binaries | Evidence |
|---|---|---|
| `trusty-search` | `trusty-search`, `trusty-embedderd` | `crates/trusty-search/Cargo.toml:32-38` |
| `trusty-memory` | `trusty-memory`, `trusty-bm25-daemon`, `trusty-memory-mcp-bridge` | `crates/trusty-memory/Cargo.toml:23-69` |
| `trusty-mpm` | `tm`, `trusty-mpm` | `crates/trusty-mpm/Cargo.toml:124-132` |
| `trusty-analyze` | `trusty-analyze` | `crates/trusty-analyze/Cargo.toml:23-25` |
| `tga` (crate name is literally `tga`) | `tga` | `crates/trusty-git-analytics/Cargo.toml:2,27` |
| `trusty-code` | `tcode` | `crates/trusty-code/Cargo.toml:16-18` |
| `trusty-installer` | `trusty-installer`, `tctl` | `crates/trusty-installer/Cargo.toml:22-31` |

`trusty-console` is a standalone crate/binary (single-owner-per-binary rule, #1318 —
the shims were deliberately removed from search/memory/mpm/analyze).

### 2.6 Workspace facts relevant to build cost
- 28 crates; `Cargo.lock` pins **1222 packages** (`grep -c '^name = ' Cargo.lock`).
- MSRV **1.91**, edition 2024 (`Cargo.toml:77-80`); CI enforces with
  `dtolnay/rust-toolchain@1.91` (`.github/workflows/ci.yml:712-721`).
- Linux CI needs only `pkg-config libssl-dev` beyond the toolchain
  (`ci.yml:168-172, 299-303`); macOS release builds use stock `macos-14` runners —
  **no extra system deps on macOS beyond Xcode CLT**.
- ONNX runtime: `trusty-embedderd` defaults to `bundled-ort`
  (`crates/trusty-embedderd/Cargo.toml:108`) — `ort-sys` downloads a prebuilt
  onnxruntime at build time (network required at compile, no system package).
- GUI crates (`trusty-mpm-gui`, `trusty-code-gui`) are excluded from default-members and
  need a pnpm/Svelte toolchain (`Cargo.toml:26-45`) — **out of scope for the harness**.
- Repo is **public** (`gh repo view bobmatnyc/trusty-tools` → `"visibility":"PUBLIC"`) —
  patterns (b) and (c) need no GitHub credentials for cloning; a token is only a
  rate-limit nicety for the installer's release-API lookups.

---

## 3. Required toolchain inventory for the VM

### 3.1 What `tahoe-base` already ships
Verified against the image's build recipe,
`cirruslabs/macos-image-templates/templates/base.pkr.hcl` (fetched via `gh api`):
- User `admin`/`admin`, SSH enabled, `~/.zprofile` sourced for login shells.
- **Homebrew** (whose installer pulls **Xcode Command Line Tools** → `git`, `clang`,
  `make`, `tar`, `shasum`).
- Brew packages: `wget unzip zip ca-certificates cmake gcc git-lfs jq yq gh
  gitlab-runner`, `node@24` + yarn, rbenv + Ruby, `awscli`, and — notably —
  **`mise` is already installed via brew** (base.pkr.hcl "brew install mise").
- **Tart Guest Agent** installed as LaunchDaemon + LaunchAgent → **`tart exec` works
  out of the box** (`tart help exec`: "all non-vanilla Cirrus Labs VM images already
  have the Tart Guest Agent installed").
- Spotlight disabled, maxfiles raised, GitHub host keys pre-seeded, TCC database
  pre-seeded for automation.
- Default VM shape from the template: 4 CPU / 8 GB / 50 GB — we should raise this with
  `tart set` (see §6).

### 3.2 What must be added (the golden bake)
| Tool | Why | How |
|---|---|---|
| Rust stable (≥1.91) | build everything; MSRV floor `Cargo.toml:80` | `mise use -g rust@stable` (mise already present; the user's `curl https://mise.run` flow also works but is redundant) |
| `mise activate` wiring | tools active in non-interactive ssh shells | append `eval "$(mise activate bash)"` to `~/.zprofile` (symlinked to `~/.profile` in the image) and `eval "$(mise activate zsh)"` to `~/.zshrc`; for scripts, prefer explicit `mise x -- cargo …` which needs no activation |
| `tmux` | trusty-mpm prereq (`prereqs/mod.rs:105-109`) | `brew install tmux` (not reliably in the mise registry) |
| `uv` | `trusty-embedderd-py` opt-in Python/MPS sidecar discovers `uv` on PATH (`crates/trusty-embedderd-py/Cargo.toml:8,46`) | `mise use -g uv@latest` |
| `claude` (optional) | trusty-mpm prereq; installer only warns if absent | `npm install -g @anthropic-ai/claude-code` (node@24 already present) — recommend **skip by default**; assert instead that the installer's missing-prereq warning path behaves |
| warmed cargo registry (optional but recommended) | avoid re-downloading ~1222 crates per run | shallow-clone repo → `cargo fetch` → delete checkout, keep `~/.cargo/registry` (see purity note §8) |

Not needed on macOS: `pkg-config`/`libssl-dev` (Linux-only, `ci.yml:168-172`), protobuf,
libgit2 (gitoxide/vendored), system onnxruntime (bundled-ort downloads its own).

---

## 4. Verdict: Cirrus CLI vs raw tart

**Verdict: script-driven with raw `tart`. Do not build on `cirrus run`.**

What `cirrus run` (verified flags, `cirrus run --help`) would genuinely give us:
- Ephemeral VM lifecycle: for a `.cirrus.yml` `macos_instance`, the CLI clones the image
  (local golden VM names resolve first — `--tart-lazy-pull` semantics), runs the task,
  deletes the VM. Matches requirement 4.
- Project directory transfer: by default the working dir is **copied into the VM
  respecting `.gitignore`** (so no `target/` leak); `--dirty` switches to a RW mount.
  The default copy is actually a reasonable pattern-(c) mechanism.
- `-e NAME[=VALUE]` env passthrough (token injection), `--artifacts-dir` for log/artifact
  collection, matrix modification for a task grid.

Why it still loses for *this* harness:
1. **Ad-hoc parametrization is the primary UX.** "Install `trusty-mpm` from branch X",
   "run released for the whole stable set", "keep the VM on failure" are CLI arguments in
   a script harness; in Cirrus they become env-var contortions inside a static YAML task
   graph, edited per run.
2. **No post-mortem.** On failure cirrus tears the VM down; debugging an install failure
   usually means SSHing into the corpse. Raw tart lets us implement `--keep`.
3. **The golden bake is out of scope for cirrus anyway.** Building
   `tahoe-trusty-toolchain` (clone, provision, stop) is imperative tart scripting; once
   those ~40 lines exist, the per-run clone/exec/delete is the same vocabulary — cirrus
   would add a second execution model on top rather than replace anything.
4. **Log capture is trivial without it** (`ssh … 2>&1 | tee logs/…`), and cirrus's
   streaming adds a CI-agent layer between us and the failure.
5. **Lifecycle opacity**: cirrus names, prunes, and caches VMs by its own conventions;
   the isolation story (§8) is easier to *prove* when every `tart clone`/`delete` is a
   line we wrote.

Keep the door open: if this harness ever graduates to scheduled infrastructure on a Mac
CI host, a thin `.cirrus.yml` can wrap the same in-VM scripts (they are plain bash taking
env vars) — nothing in the recommended design precludes it.

---

## 5. Recommended architecture

**Form: bash. Location: `scripts/vm-install/`.**

Why bash and not a Rust `crates/trusty-vm-test` or xtask:
- `Cargo.toml:16` globs `crates/*` into the workspace — a harness crate would be built,
  clippy-gated, and MSRV-checked by `make check` and CI, violating requirement 5; opting
  out requires `exclude` machinery and default-members maintenance the workspace
  explicitly complains about (`Cargo.toml:47-56`).
- The work is 95% process orchestration (tart, ssh, tar) — bash's home turf, and the repo
  already establishes the pattern with `scripts/e2e-docker.sh` (host driver) +
  `docker/e2e/smoke.sh` (guest payload).
- No compile step before an ad-hoc run.

```
scripts/vm-install/
├── vmtest                  # entry point (bash). Subcommands: bake, run, list, clean
├── lib/
│   ├── vm.sh               # OS-specific boundary: vm_clone/vm_start/vm_wait/vm_ip/
│   │                       #   vm_exec/vm_ssh/vm_push/vm_destroy — tart-macOS impl today;
│   │                       #   a future vm_linux.sh implements the same functions
│   └── scenario.sh         # scenario = ordered list of steps; step runners:
│                           #   step_install_released|branch|local, step_verify
├── remote/                 # scripts executed INSIDE the VM (pushed over ssh)
│   ├── provision.sh        # golden bake payload (mise, rust, tmux, uv, cargo fetch)
│   ├── install-released.sh # install.sh path + cargo-install variant
│   ├── install-source.sh   # shared by branch/local: cargo install --path per crate
│   └── verify.sh           # success criteria (§7); modeled on docker/e2e/smoke.sh
├── scenarios/
│   ├── released-stack.sh   # full stable set via install.sh
│   ├── released-crate.sh   # single crate via cargo install
│   ├── branch-crate.sh
│   └── local-crate.sh
└── README.md
```

Invocation surface (all manual, never wired into CI/`make check`):

```
scripts/vm-install/vmtest bake                       # one-time golden image
scripts/vm-install/vmtest run released               # install.sh full-stack path
scripts/vm-install/vmtest run released --crate trusty-search --version 0.24.10
scripts/vm-install/vmtest run branch  --crate trusty-mpm --ref my-branch
scripts/vm-install/vmtest run local   --crate trusty-mpm [--keep]
```

---

## 6. Concrete implementation sketch (runnable commands)

### 6.1 Shared plumbing (`lib/vm.sh`)

```bash
GOLDEN=tahoe-trusty-toolchain
KEY="${HOME}/.local/state/trusty-vmtest/id_ed25519"   # harness-owned keypair
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o ConnectTimeout=5 -o LogLevel=ERROR)

vm_boot() {  # $1 = vm name
  tart run "$1" --no-graphics &            # window-less; VM stops when deleted
  # Guest agent is preinstalled in cirruslabs images → tart exec is our readiness probe
  until tart exec "$1" true 2>/dev/null; do sleep 2; done
}

vm_authorize() {  # inject harness pubkey; solves non-interactive SSH without sshpass
  [ -f "$KEY" ] || ssh-keygen -t ed25519 -N '' -f "$KEY" -C trusty-vmtest
  tart exec -i "$1" /bin/sh -c \
    'mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys' \
    < "${KEY}.pub"
}

vm_ssh()  { ssh  "${SSH_OPTS[@]}" -i "$KEY" "admin@$(tart ip "$1")" "${@:2}"; }
```

Host-key churn is a non-issue with `StrictHostKeyChecking=no` +
`UserKnownHostsFile=/dev/null`; each clone gets a fresh NAT IP from the vmnet DHCP.
(`tart set --random-mac` on the clone if two runs must coexist — same MAC would collide.)

### 6.2 Golden bake (one-time; **no secrets, no trusty artifacts**)

```bash
tart clone tahoe-base "$GOLDEN"
tart set "$GOLDEN" --cpu 8 --memory 12288 --disk-size 80
vm_boot "$GOLDEN"; vm_authorize "$GOLDEN"
vm_ssh "$GOLDEN" 'bash -s' < remote/provision.sh
vm_ssh "$GOLDEN" 'sudo shutdown -h now' || true    # or: tart stop "$GOLDEN"
```

`remote/provision.sh` (runs in guest; base image's brew is on PATH via `~/.zprofile`):

```bash
set -euxo pipefail
source ~/.zprofile                                  # brew shellenv (baked by cirruslabs)
# mise is preinstalled by brew in tahoe-base; wire activation for all shell types
grep -q 'mise activate' ~/.zprofile || echo 'eval "$(mise activate bash)"' >> ~/.zprofile
touch ~/.zshrc; grep -q 'mise activate' ~/.zshrc || echo 'eval "$(mise activate zsh)"' >> ~/.zshrc
mise use -g rust@stable uv@latest
mise x -- rustc --version                           # sanity: >= 1.91 (workspace MSRV)
brew install tmux                                   # trusty-mpm prereq
# Warm the cargo registry against the workspace's lockfile, then remove all sources:
git clone --depth 1 https://github.com/bobmatnyc/trusty-tools /tmp/warm
(cd /tmp/warm && mise x -- cargo fetch)
rm -rf /tmp/warm                                    # golden keeps ~/.cargo/registry ONLY
```

### 6.3 Per-run lifecycle (every pattern)

```bash
VM="vmtest-$(date +%Y%m%d-%H%M%S)"
tart clone "$GOLDEN" "$VM"                          # APFS CoW: seconds, ~0 extra disk
trap '[ -n "${KEEP:-}" ] || { tart stop "$VM" 2>/dev/null; tart delete "$VM"; }' EXIT
vm_boot "$VM"
mkdir -p "logs/$VM"
```

All guest commands run as
`vm_ssh "$VM" '…' 2>&1 | tee "logs/$VM/step-N.log"`; the run fails on first non-zero
exit (`set -euo pipefail` in the driver).

### 6.4 Pattern (a) — released

Primary (real user path, prebuilt-first — no compile):

```bash
vm_ssh "$VM" 'export TRUSTY_YES=1;
  curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh'
```

Optionally pass a rate-limit token without persisting it (see §6.7). Secondary variant
(source-of-truth parity with `docker/e2e/Dockerfile:50-77`):

```bash
vm_ssh "$VM" "source ~/.zprofile
  export CARGO_TARGET_DIR=\$HOME/build TRUSTY_SKIP_RAM_CHECK=1
  mise x -- cargo install ${CRATE}${VERSION:+ --version $VERSION} --locked"
```

`CARGO_TARGET_DIR` is the single most important cost lever: without it every
`cargo install` builds its full dep tree in a private temp target dir.

### 6.5 Pattern (b) — branch

Clone-once + `--path` install (preferred over `cargo install --git` so multiple crates
share one checkout and one target dir; repo is public, no auth needed):

```bash
vm_ssh "$VM" "source ~/.zprofile
  git clone --branch '$REF' --single-branch --depth 1 \
    https://github.com/bobmatnyc/trusty-tools ~/src/trusty-tools
  cd ~/src/trusty-tools
  export CARGO_TARGET_DIR=\$HOME/build
  mise x -- cargo install --path crates/$CRATE --locked"
```

(`cargo install --git https://github.com/bobmatnyc/trusty-tools --branch "$REF" $CRATE
--locked` is a valid one-crate shortcut; keep it as a flag, not the default.)

### 6.6 Pattern (c) — local (host source → VM)

Mechanism comparison:

| Mechanism | Speed | Isolation | Uncommitted changes | `target/` leak |
|---|---|---|---|---|
| `--dir` mount RW (virtiofs) | instant | **BROKEN — guest `cargo` writes to host `target/`**; guest can mutate the host repo | yes | yes (both directions) |
| `--dir` mount RO | instant setup; slow many-small-file reads over virtiofs | good (guest cannot write; `cargo install --path --locked` never needs to) | yes | host `target/` visible but unused |
| `git archive`/bundle over ssh | seconds | perfect (copy) | **no** — committed tree only | no |
| **tar of `git ls-files -co --exclude-standard` over ssh (recommended)** | seconds (repo sans ignored files is tens of MB) | perfect (copy; build on guest-native APFS) | **yes** — tracked + untracked-unignored, exactly the working tree minus `.gitignore` | no by construction |

Recommended default:

```bash
( cd "$REPO_ROOT" && git ls-files -co --exclude-standard -z | tar -czf - --null -T - ) \
  | vm_ssh "$VM" 'mkdir -p ~/src/trusty-tools && tar -xzf - -C ~/src/trusty-tools'
vm_ssh "$VM" "cd ~/src/trusty-tools
  export CARGO_TARGET_DIR=\$HOME/build
  source ~/.zprofile && mise x -- cargo install --path crates/$CRATE --locked"
```

Offer `--mount-ro` (`tart run "$VM" --no-graphics --dir "src:$REPO_ROOT:ro,tag=src"` +
`mount_virtiofs src ~/hostsrc` in guest) as an opt-in for rapid iteration; **never**
implement a RW mount path.

### 6.7 GH token injection (optional; rate limits only — repo is public)

Rule: **the bake step runs zero commands that reference secrets**, so no image can ever
contain one. Per-run, pass the token over the ssh channel via stdin (never argv — argv is
visible in guest `ps`; never a file — even though the VM is deleted, discipline is
cheaper than exceptions):

```bash
gh auth token | vm_ssh "$VM" 'IFS= read -r GH_TOKEN; export GH_TOKEN GITHUB_TOKEN=$GH_TOKEN
  curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | TRUSTY_YES=1 sh'
```

`install.sh:186-192` picks up `GITHUB_TOKEN`/`GH_TOKEN` for its release-API calls. If a
future private dependency appears, the same stdin pattern feeds
`gh auth login --with-token` inside the ephemeral clone. Do not use ssh agent forwarding
(`-A`) — that hands the guest a signing oracle for host keys.

---

## 7. Success / verification criteria (`remote/verify.sh`)

Layered, all patterns share the same script; a pattern passes only if every layer passes.

1. **Install exit code 0** — for pattern (a)/install.sh this already implies the
   installer's own `ensure` + `stack health` verify tail passed (`install.sh:630-643`,
   `verify_tail.rs`).
2. **Binary presence + runnability** — for each crate installed, `command -v <bin>` and
   `<bin> --version` for **every** Single-Install binary from the table in §2.5 (e.g.
   installing `trusty-memory` must yield `trusty-memory`, `trusty-bm25-daemon`, and
   `trusty-memory-mcp-bridge` — this directly tests the convention).
3. **Version assertion** — released: reported version equals the pinned `--version` (or
   crates.io latest); branch/local: equals the `version =` in the checkout's crate
   manifest (guards against a stale prebuilt or PATH shadowing; cf. the installer's own
   `shadow_check.rs`).
4. **Daemon start + health** (ported from `docker/e2e/smoke.sh:76-120`):
   `TRUSTY_SKIP_RAM_CHECK=1 trusty-search start`, poll
   `http://127.0.0.1:$(trusty-search port)/health`, index a small fixture with
   `--lexical-only` (no ONNX/model download), assert a query hit. Analogous minimal
   checks per daemon; launchd members additionally assert their plist landed under
   `~/Library/LaunchAgents` and `launchctl print gui/$(id -u)/<label>` shows it loaded.
5. **MCP registration** — in a scratch project dir, run `tctl ensure`, then assert
   `.mcp.json` contains the expected `mcpServers` keys (`ensure/mcp_patch.rs:23-30`).
   (Installer-driven pattern only; `cargo install` variants assert step 2-4 only.)
6. **`tctl status` / `tctl stack health`** exit 0 when the full stack was installed.

Output contract: verify.sh prints `[PASS]/[FAIL]/[SKIP]` lines and a summary (same style
as smoke.sh), exits non-zero on any FAIL; the host driver copies `~/vm-logs` +
daemon logs back via `scp` **before** the trap deletes the VM.

---

## 8. Isolation analysis

Host touchpoints of a real trusty install, and how the VM boundary covers each:

| Touchpoint on a real host | In this design |
|---|---|
| `~/.cargo/bin`, `~/.cargo/registry` | guest-only; host cargo untouched |
| `~/.local/bin` (trusty-installer, tctl) | guest-only (`install.sh:52`) |
| `~/.local/share`/`XDG_DATA_HOME` trusty state | guest-only |
| `~/Library/LaunchAgents/*.plist` + launchd services | guest launchd; host launchd never sees them |
| Daemon TCP ports (7878/7879/…) | guest binds 127.0.0.1 **inside the VM's NAT network** — cannot collide with host daemons |
| `~/.claude` / project `.mcp.json` | guest-only; `tctl ensure` runs in a guest scratch dir |
| `~/.trusty-mpm`, tmux sessions | guest-only |
| macOS TCC/FDA grants, codesigning | guest TCC database; host grants unaffected (signed-install scripts are host-only tooling, out of scope) |
| Host repo `target/` | **the one real hazard** — only reachable via a RW `--dir` mount, which is banned; default transfer is a copy, `--mount-ro` is read-only |
| Host credentials | none persisted: bake is secret-free, tokens flow per-command over stdin, no agent forwarding |
| Host side effects the harness *does* have | `~/.tart` disk usage (CoW clones + golden image ~15-30 GB; `vmtest clean` prunes), transient CPU/RAM while a VM runs, outbound network to crates.io/GitHub |

Prebaking purity: the golden image contains rust/mise/tmux/uv and a cargo **registry
cache**. The registry cache is a content-addressed mirror of crates.io sources — it
contains no compiled trusty code, no trusty binaries, no trusty runtime state, and cannot
influence *whether* an install succeeds (only how fast). If absolute purity is wanted for
a release-gate run, `vmtest run --no-warm-cache … ` can clone straight from `tahoe-base`
instead — the design keeps both.

---

## 9. Time/cost estimates and mitigations

Fixed per-run overhead: `tart clone` (APFS CoW) ≈ 5–15 s; boot to guest-agent-ready
≈ 30–60 s; key inject + housekeeping ≈ 10 s; delete ≈ seconds. **~1–2 min total.**

One-time golden bake: rust via mise ≈ 2–4 min; `cargo fetch` of 1222 packages ≈ 2–5 min;
brew tmux + wiring ≈ 2 min. **~10–20 min once**, amortized. Per-run saving vs
provisioning from `tahoe-base` each time: **~8–12 min + removal of three network-flake
classes** (mise/rustup CDN, crates.io index, brew).

Per-pattern wall clock (8 vCPU / 12 GB guest on Apple Silicon; VM CPU ≈ near-native per
core but fewer cores than host):

| Pattern | Estimate | Notes |
|---|---|---|
| (a) install.sh prebuilt path, full stack | **3–8 min** | downloads GitHub-Releases tarballs; zero compile. This is why it should be the default released scenario |
| (a) `cargo install` single crate | 20–45 min | full release build of ~600–1000 deps |
| (a) `cargo install` 4-crate set, naive | 1.5–3 h | what docker-e2e does nightly; each install rebuilds deps |
| (a) 4-crate set with shared `CARGO_TARGET_DIR` | 45–90 min | first crate pays, the rest reuse dep artifacts |
| (b)/(c) single crate from source | 20–45 min | dominated by dependency compilation |
| (b)/(c) full stable set from one checkout, shared target | 45–90 min | feasible but not something to run ten times a day |

Verdict on feasibility: **from-source full-stack installs are feasible but slow; the
harness must make single-crate the default scope** and full-stack an explicit flag.

Mitigations (in priority order):
1. Always `--locked` (already the repo convention everywhere: Dockerfile, installer,
   signed scripts).
2. Shared `CARGO_TARGET_DIR=$HOME/build` across all installs in a run (§6.4).
3. Prebaked registry cache (bake step) — kills the fetch phase.
4. Default `--crate` scope; full stack behind `--all`.
5. `cargo install --profile dev` opt-in (`--fast`) for functional checks — roughly
   halves compile time, with the caveat that it is not the bit-for-bit user path; never
   use it for release-gate runs.
6. If iteration speed ever matters more: sccache with its cache dir refreshed into the
   golden image periodically — deliberately **not** in v1 (it drags trusty-derived
   compile artifacts into the golden image, weakening the purity story of §8).
7. Do not go below 12 GB guest RAM (avoids both link-time pressure and the
   `TRUSTY_SKIP_RAM_CHECK` guard being load-bearing; still export it, as docker-e2e does).

---

## 10. Extension points

**Upgrade testing** (secondary req 1): a scenario is already an ordered list of steps
(`lib/scenario.sh`), so upgrade becomes data, not new machinery:

```
scenario "upgrade-released-to-local" \
  step_install_released trusty-mpm 0.6.2 \
  step_verify \
  step_install_local trusty-mpm \
  step_verify --expect-restarted trusty-mpm
```

The only new primitive needed later is `--expect-restarted` (assert daemon PID/start-time
changed and version bumped — the installer's `upgrade_and_restart` behavior,
`stable_set.rs:60-66`, gives the semantics to check against).

**Linux** (secondary req 2): everything OS-specific lives behind `lib/vm.sh`'s six
functions. Tart itself runs Linux guests on Apple Silicon
(`ghcr.io/cirruslabs/ubuntu:*`), so `vm_clone/vm_ip/vm_exec` survive unchanged; the
Linux deltas are confined to: provision.sh (apt + mise instead of brew), virtiofs mount
syntax (`mount -t virtiofs`), and verify.sh's launchd assertions (systemd user units /
whatever trusty ships on Linux). The AL2023 build workflow
(`.github/workflows/al2023-build.yml`) and the docker-e2e image are the reference for
Linux package prerequisites (`pkg-config`, `libssl-dev`, `cmake`).

---

## 11. Open questions / risks

1. **TCC prompts inside the VM.** The cirruslabs base image pre-seeds the TCC database
   for automation, and daemons operating on `$HOME`-local fixtures should not need FDA —
   but a first `trusty-mpm`/tmux or launchd bootstrap inside a fresh macOS Tahoe guest
   may still surface a consent dialog with no display attached. First real run should be
   done once with graphics on (`tart run` without `--no-graphics`) to observe; if a
   prompt appears, extend the bake's TCC seeding.
2. **GitHub API rate limits** on the installer's release lookup (60/hr unauthenticated,
   HTTP 403 — `install.sh:176-210`). Ephemeral VMs share the host's egress IP; repeated
   runs can exhaust it. The §6.7 token pipe is the fix; make it the default when
   `gh auth token` succeeds on the host.
3. **`tart exec` fidelity.** Readiness-probe and key-injection usage is minimal and
   verified against `tart help exec` (v2.32.1, `-i` stdin supported), but exit-code and
   large-stdout behavior under the guest agent should be smoke-tested once before
   relying on it beyond bootstrap. Fallback: password auth + `expect` one-liner purely
   for the key injection step.
4. **Disk sizing.** 50 GB default minus OS leaves ~15–20 GB free; a shared release
   target dir for the full workspace plus registry can consume 15–25 GB. The bake's
   `tart set --disk-size 80` needs the guest to auto-grow the APFS container (recent
   tart/cirruslabs images handle this; verify once).
5. **Golden image drift.** rust@stable moves; the golden image pins whatever was current
   at bake time. Record `rustc --version` + image build date in a
   `tahoe-trusty-toolchain.meta` file next to the harness; `vmtest bake --rebake`
   refreshes. MSRV runs (1.91) can be a scenario variant (`mise use -g rust@1.91`
   per-run — small delta, tool downloads are cached by mise only if prebaked; acceptable
   as a rare run).
6. **Parallel runs.** Two clones share the golden image safely (CoW) but need
   `tart set --random-mac` to avoid DHCP identity collision; v1 can simply lock to one
   run at a time.
7. **`trusty-code` / `cto-assistant` / other non-stable-set crates.** The harness's
   crate table (§2.5) covers them for `cargo install` patterns, but they have no
   installer/prebuilt path; scenario coverage beyond the stable set is a data edit in
   `remote/verify.sh`'s expected-bins map.
