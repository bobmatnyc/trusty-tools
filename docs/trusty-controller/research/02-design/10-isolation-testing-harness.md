# DOC-10 — Isolation Testing Harness (MUC1, MUC2)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-5](./05-controller-cli.md) (Accepted), [DOC-8](./08-install-bootstrap.md) (Accepted), [DOC-9](./09-upgrade-flow.md) (Accepted), [DOC-4](./04-doctor-health-rollup.md) (Accepted), [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted)
**Gated by:** [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted) — only conformant members can be installed/rolled-up/asserted; `tctl doctor --self-check` (DOC-6 §8) is reused as the per-member conformance gate.
**Cross-ref:** [DOC-0](./00-naming-and-doc-charter.md), [DOC-3](./03-scope-model.md) (Accepted)

## Purpose

Specify how a maintainer tests install/upgrade of the whole stack in a vanilla
container/VM — **macOS primary, Linux secondary** (MUC2) — without contaminating
their own machine (MUC1). This is the **capstone validation doc**: it does not
introduce new product behaviour; it composes the already-accepted flows DOC-8
(install), DOC-9 (upgrade) drive through the DOC-5 CLI and asserts on the DOC-4
`--json` rollup over the DOC-2 BOM. The harness is the executable expression of
the "isolation-testability" cross-cutting requirement that DOC-8 §Cross-cutting
and DOC-9 §Cross-cutting both promise (every flow runnable non-interactively and
side-effect-scoped so it can run in a clean VM/container).

The harness is also where two deferrals from upstream docs are **resolved**:

- **DOC-8 Q6 / Resolved Decision 6** deferred the *exact* Linux daemon-supervision
  detail (systemd-user unit content vs foreground/daemonless) to DOC-10. §4 resolves
  it for the harness.
- **DOC-2 Q6 / DOC-6 §5** deferred the *concrete* claude-mpm version pin "to
  implementation" — i.e. to the harness that tests it. §6 resolves the *mechanism*
  by which the harness pins it.

---

## DESIGN

### 1. Why isolation (MUC1) — the anti-pattern this harness exists to avoid

The spec is explicit (spec §167–170): a maintainer must test the install process
in a **vanilla container or VM**, and *"using maintainer's system and per project
setup (maintainer is also the user of installed stack) is an anti-pattern."* The
reason is that the entire value proposition under test is **UUC2 — the
zero-knowledge install** (spec §156): a user *"with zero knowledge … of … setup
requirements"* runs one command and lands on a working stack. A maintainer's own
machine is the worst possible place to validate that, because it already has:

- **cargo / rustup** installed (so DOC-8 §5's hard-dependency guide-and-abort path
  never fires and is never exercised),
- **uv** installed (same — the orchestrator hard-dep §5 path is never seen),
- **stack binaries already on PATH** (so `tctl install` short-circuits to its
  idempotent no-op §1.5 instead of doing a real install),
- **pre-existing `~/.config/trusty-controller/` state, launch agents, indexes,
  palaces, and `.mcp.json` entries** (so "configured/exists/fresh" project ladder
  rungs are already climbed and the ensure pass §2 never does real work),
- a populated **embedding-model cache** and warm CARGO build caches.

Every one of those contaminants hides a real first-run failure from the maintainer.
The only faithful test of "vanilla machine → ready stack" is one that *starts*
vanilla: **no cargo, no uv, no tool binaries, no controller state, a clean `$HOME`,
and a clean PATH.** The harness's first responsibility is therefore to **construct
that vanilla environment**, and only then drive the flows.

A subtle but load-bearing consequence (§3): on **macOS a clean `$HOME` is NOT true
isolation**, because launchd is *per-user*, not per-`$HOME`. A separate `$HOME`
shares the same `gui/$(id -u)` launchd domain as the maintainer's real session, so
a launch agent the harness installs (DOC-8 §1.1 step 4) lands in the maintainer's
own launchd domain and can collide with — or leak into — their running daemons.
True macOS isolation requires a separate user session, i.e. a **VM** (§3).

### 2. The core scenario (the end-to-end acceptance run)

The harness drives **one canonical scenario** end to end. It is the literal
composition of UUC2 → UUC1 → UUC3, each asserted via the DOC-4/DOC-5 `--json`
contract. Every step runs **non-interactively** (`--yes`, non-TTY) per DOC-5 §3.3
and asserts on machine output, never scraped human text.

```
                ┌─────────────────────────────────────────────────────────┐
                │  VANILLA ENV (clean $HOME, no cargo/uv/tools — §3 / §4)   │
                └─────────────────────────────────────────────────────────┘
                                          │
  STEP 0  bootstrap toolchain            ▼
          install rustup (cargo) + uv as the FIRST harness step.
          (This is the harness pre-provisioning the DOC-8 §5 hard deps; it
           also lets the harness optionally exercise the guide-and-abort path
           by running `tctl install` BEFORE this step — see §5 assertion A0.)
                                          │
  STEP 1  install the controller         ▼
          cargo install trusty-controller --locked     (cdhash-safe, §3 / §7)
          → `tctl` is now on PATH; it carries the embedded BOM (DOC-2 §2).
                                          │
  STEP 2  UUC2 — zero-knowledge install  ▼
          tctl install --yes
          → ASSERT exit 0; then
          tctl stack doctor --json
          → ASSERT verdict == "ready", exit_code == 0, every system cell ready,
            summary.down == 0.                            (oracle: §5 / DOC-4 §8.2)
                                          │
  STEP 3  UUC1 — per-project ensure      ▼
          cd <sample repo>;  tctl ensure --scope project --wait --yes
          → ASSERT exit 0; index reaches `fresh` (--wait, DOC-8 §3.3 / Q3); then
          tctl stack doctor --json
          → ASSERT trusty-search project cell == "ready" (not just "pending"),
            .mcp.json patched, palace exists.
                                          │
  STEP 4  idempotency re-run             ▼
          tctl install --yes   AND   tctl ensure --scope project --yes
          → ASSERT both report no-op (already-installed / already-ready),
            exit 0, and stack doctor STILL ready.        (DOC-8 §1.5 / §2.3)
                                          │
  STEP 5  UUC3 — upgrade N-1 → N         ▼
          (the env was seeded at stack tuple N-1 — see §7 "which BOM")
          tctl updates --json    → ASSERT lists the expected member deltas
          tctl upgrade --yes     → ASSERT exit 0
                                          │
  STEP 6  verify-after (take-effect)     ▼
          tctl version --json    → ASSERT stack_version == N
          tctl stack doctor --json → ASSERT verdict "ready", exit 0
          per upgraded member: <binary> version --json .tool_version == target
            AND <binary> health --json status == "running"   (DOC-9 §7)
          → the NEW versions are actually LIVE, not merely installed-on-disk.
```

#### 2.1 The pass/fail oracle (what "green" means, precisely)

The harness asserts on **machine fields only**, so the oracle is unambiguous and
non-racy:

| Gate | Command | Asserted field(s) | Pass condition |
|---|---|---|---|
| install succeeded | `tctl install --yes` | process exit code | `0` |
| stack healthy (system) | `tctl stack doctor --json` | `verdict`, `exit_code`, `summary.down` | `verdict == "ready"`, `exit_code == 0`, `summary.down == 0` (DOC-4 §7) |
| each member up | (embedded per-member `cells.system.verdict`) | per DOC-4 §8.2 | all `ready` |
| BOM versions live | `tctl version --json` + per-member `version --json` | `stack_version`, each `tool_version` | equal the target BOM pins (DOC-2) |
| daemons running | per-member `health --json` | envelope `status` | `running` (DOC-4 §2.0) |
| project ready (non-racy) | `tctl ensure --wait` then `stack doctor --json` | `cells.project.verdict` for two-layer members | `ready` (the `--wait` gate makes this deterministic — §5) |
| `.mcp.json` patched | filesystem read of the project `.mcp.json` | presence of the member entries | each enabled two-layer member's MCP server entry present |
| idempotent | second `tctl install`/`ensure` | exit code + reported action | `0` + "no-op / already" (DOC-8 §1.5, §2.3) |
| upgrade took effect | post-upgrade `version --json` + `health --json` | `tool_version`, `status` | target version + `running` (DOC-9 §7 verify-after) |

A run is **green** iff every gate above passes. Anything else is a hard failure
with the failing member/cell surfaced from the DOC-4 `clusters[]`/`remediations[]`.
Because exit codes are **system-track-driven** (DOC-4 §7), a transient project
`pending` *before* the `--wait` completes is NOT a failure — the oracle only reads
the project cell after `--wait` returns `fresh`, eliminating the race the spec asks
about (spec §150–151).

#### 2.2 Conformance pre-gate (DOC-6 gating edge)

Before asserting stack health, the harness runs DOC-6 §8's
**`tctl doctor --self-check <member>`** for each installed member (including the
claude-mpm shim). This is the DOC-6 acceptance gate: a member that passes is, by
construction, conformant for its advertised verbs, so the §2.1 oracle is reading
trustworthy envelopes. The self-check is the bridge that lets DOC-10 "only validate
conformant tools" (DOC-6 §Produces / "gates DOC-10").

The pre-gate also validates each member's real `--json` envelope and per-verb
`data` output against the **committed golden contract schema** exported from the
`trusty_common` contract module (the same fixtures that gate the Rust
golden-snapshot test — DOC-1 D3 / ADR-0007). This extends snapshot coverage to
the **non-Rust claude-mpm shim**, which hand-rolls its JSON via the Python
adapter and is therefore *not* gated by the Rust snapshot test: capturing its
live output and asserting it against the golden schema holds it to the same
`data`-shape contract as the Rust tools, closing the version-skew channel (a tool
shipping a changed `data` shape under a stale `contract_version`) for the one
member the in-workspace shared types cannot cover.

### 3. macOS isolation (PRIMARY, MUC2) — resolve the launchd-per-user problem

macOS is the **primary** target (spec §172–173). The defining constraint is
launchd's per-user domain (DOC-8 §6, root CLAUDE.md): launch agents load into
`gui/$(id -u)` via `launchctl bootstrap`, keyed on the **uid**, not on `$HOME`.

#### 3.1 Why a clean `$HOME` is not enough — recommend a VM

A `HOME=$(mktemp -d)` "ephemeral home" gives the harness a clean config/state dir
and a clean `.mcp.json` location, **but it shares the maintainer's uid and therefore
the maintainer's launchd domain.** Installing a stack daemon's launch agent under
that domain (DOC-8 step 4) means:

- the agent's `<label>` can collide with the maintainer's already-loaded daemon of
  the same label (a `bootstrap` over an existing label fails or replaces the live
  one);
- a failed teardown leaks a launch agent into the maintainer's real session;
- port binding (`trusty-search` auto-binds, DOC-2 `ui.port_source`) can contend
  with the maintainer's running daemons.

This is the precise sense in which "clean `$HOME` ≠ isolation" on macOS. An
**ephemeral macOS *user account*** would isolate launchd (its own
`gui/$(id -of-new-user)`), but creating/destroying real user accounts per test is
heavyweight, requires admin, and still shares the kernel, the system-wide cargo
install location, and the model cache. **Recommendation: use a VM.** A VM gives a
genuinely separate uid + login session (its own launchd domain), a clean PATH, a
clean `~/.cargo`, and disposable teardown — the only faithful macOS isolation.

#### 3.2 Tooling recommendation — `tart` (with a documented fallback)

Recommendation (see Open Question 1 — owner picks the canonical tool):

- **Primary recommendation: [`tart`](https://tart.run) (Cirrus Labs).** Rationale:
  tart is purpose-built for **macOS-on-Apple-Silicon VMs** using the native
  `Virtualization.framework`, it is the de-facto standard for macOS CI (Cirrus,
  self-hosted GitHub runners), it has a clean `tart clone / run / stop / delete`
  lifecycle ideal for "provision → run scenario → tear down", and prebuilt vanilla
  macOS base images are published. It runs on the same Apple-Silicon hardware the
  team already develops on, so no x86 emulation tax. tart's license is free for
  small numbers of VMs (the harness uses one at a time).
- **Fallback / alternatives, ranked:**
  - **`lima`** — excellent for Linux VMs on macOS (and is what §4 could use for the
    Linux leg on a macOS host) but does **not** run macOS guests; not a fit for the
    macOS *primary* leg.
  - **UTM** — GUI-first (QEMU/Virtualization.framework); scriptable via
    `utmctl` but less CI-ergonomic than tart for headless provision/teardown.
  - **Anka (Veertu)** — robust, CI-grade macOS virtualization, but **commercially
    licensed**; only worth it if the team already has Anka infrastructure.

**Cost / CI-availability note (the real constraint):** macOS VMs are expensive to
run in CI. Two routes exist and the project already touches both:
- **GitHub-hosted `macos-14` runners** — the release workflow (`release.yml`,
  verified) already builds on `macos-14` (Apple-Silicon, arm64). A GitHub-hosted
  macOS runner is itself effectively a *fresh, ephemeral macOS host* with its own
  uid/login session, so for CI purposes the runner **is** the isolation VM — the
  harness can run the §2 scenario directly on the runner without nesting a tart VM
  inside it (nested virtualization on hosted runners is restricted anyway). macOS
  runner minutes bill at a higher multiplier than Linux, which is why §6b
  recommends running the macOS leg **nightly/manual**, not on every PR.
- **Self-hosted Apple-Silicon + tart** — for frequent local maintainer runs, a
  maintainer runs `tart` on their own Mac (or a dedicated build Mac), getting the
  separate-uid isolation §3.1 requires without GitHub minutes.

**So:** macOS isolation = a fresh macOS *login session with its own uid*. In CI that
is a `macos-14` GitHub runner (run the scenario directly on it); locally/self-hosted
that is a `tart` VM clone. Both satisfy MUC2's "isolated macOS install."

#### 3.3 Baseline image + first harness step (exercise DOC-8 §5)

The macOS base image is a **vanilla macOS** (no Xcode-CLT-installed rustup, no uv,
no Homebrew-installed stack tools). The harness's **STEP 0** (§2) installs the
toolchain *inside* the VM as its first action:

```
# inside the VM (vanilla macOS), as the harness's first provisioning step:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y   # cargo
curl -LsSf https://astral.sh/uv/install.sh | sh                          # uv
```

This rustup+uv first step is the **executable mirror of DOC-8 §1.1's STEP 0**
product on-ramp (install Rust via the rustup one-liner, then `cargo install
trusty-controller`): the harness is the executable expression of the same
vanilla-machine bootstrap, so the product flow is no longer harness-only and the
two are aligned.

This is deliberate: by *starting* without cargo/uv, the harness can **first**
assert DOC-8 §5's guide-and-abort UX (§5 assertion A0) — run `tctl …` with the
toolchain absent and confirm exit `3` + the copy-paste remediation — and **then**
provision the toolchain and proceed. The image needs only a base macOS + `curl`
(present by default). Xcode Command Line Tools (for a linker) are the one
heavyweight prerequisite the base image should pre-bake, since `cargo install`
needs a C linker; baking CLT into the base image is acceptable (it is not part of
the "zero-knowledge user setup" surface — a Mac that can run cargo at all has it).

#### 3.4 cdhash caveat inside the VM (load-bearing)

DOC-8 §1.3 / DOC-9 §3.3 / root CLAUDE.md: on macOS, **never `cp` a binary into
PATH** — `cargo install`'s atomic-rename keeps the kernel's code-signing (`cdhash`)
cache consistent; a `cp` over an on-PATH binary can SIGKILL the next exec with
`EXC_CRASH / CODESIGNING` (looks like an OOM, is not). **The harness honours this
inside the VM exactly as a real install would:** it installs/upgrades every cargo
member via `tctl install`/`tctl upgrade` (which compose `cargo install … --locked`,
DOC-8 §1.3), and the harness's own STEP 1 controller bootstrap is
`cargo install trusty-controller --locked` — **never** a `cp` of a pre-built binary
from the host into the VM's PATH. This means the harness is also a regression gate
*for the cdhash-safety property itself*: if a future change introduced a copy-into-PATH
shortcut, the post-install `health --json` probe would catch the SIGKILL'd daemon
as `down`.

### 4. Linux isolation (SECONDARY) — resolve DOC-8 Q6 for the harness

Linux is **secondary but important** (spec §173). Linux isolation is far simpler
than macOS because **there is no launchd and no cdhash cache** (DOC-8 §6): a Docker
container gives a clean PATH, clean `$HOME`, clean state dirs, and disposable
teardown with none of the per-user-domain complications. The project already runs a
container-based CI job (`al2023-build.yml`: `container: amazonlinux:2023`, rustup
installed inside), so the pattern is grounded.

#### 4.1 Resolving DOC-8 Q6 (systemd-user vs foreground) — for the harness

DOC-8 Resolved Decision 6 set the **product** Linux v1 target as *systemd-user unit
where `systemctl --user` is available, foreground/daemonless fallback otherwise*,
and explicitly deferred the harness-level choice to DOC-10. The resolution:

- **For the Linux *container* harness: use foreground/daemonless `serve` (no init
  system).** A stock Docker container has **no systemd** (PID 1 is the entrypoint,
  not `systemd`; `systemctl --user` requires a running user session bus + logind,
  which a plain container lacks). Standing up systemd-in-a-container (systemd as PID
  1, `--privileged`, cgroup mounts) is heavy, brittle, and tests the *supervisor*
  rather than the *stack*. Since the daemons already support a foreground/daemonless
  mode (`<binary> serve` / `start --foreground`, the exact pattern the trusty-search
  baseline suite uses — `trusty-search start --foreground &`), the harness supervises
  daemons itself: start each daemon in the background within the container, wait for
  `health --json` → `running`, run the scenario, then `stop`. This isolates the
  test from any init system and keeps the container minimal.
  - **Implication for `tctl`:** on Linux-in-container the controller's daemon
    supervision step (DOC-8 §1.1 step 4) takes the **foreground fallback branch**
    (DOC-8 §6: "daemonless / foreground"), which the harness must support driving
    non-interactively. `tctl restart` on this branch is stop+start of the foreground
    process (DOC-9 §5.1), not `systemctl --user restart`.
- **systemd-user is validated separately from the foreground container leg — and now
  gated per-PR on a standard ubuntu runner.** A stock Docker container has no systemd
  (the reason above), so the *systemd-user* product path (DOC-8 §6 primary Linux
  branch) cannot run inside the every-PR container. But a **standard GitHub-hosted
  ubuntu runner is a real login session and has `systemctl --user`**, so the
  systemd-user product path is now gated **per-PR on that runner** (path-filtered,
  alongside the foreground-container leg) — no privileged systemd-in-container or full
  VM is needed. So per-PR Linux coverage is **both** legs: the systemd-less container
  proves the foreground/fallback branch ("install → ready → upgrade → still-ready"
  under foreground supervision), and the ubuntu-runner leg proves the real systemd-user
  product path including the unit content + a `systemctl --user restart`. The **full
  Linux VM** (lima/cloud VM/self-hosted) becomes a deeper/optional **nightly**
  validation atop the per-PR runner leg, not the sole systemd-user gate. (See Open
  Question 2 — whether v1 ships the systemd-user VM leg at all, or defers it now that
  the runner leg covers the product path on every PR.)

#### 4.2 Container base image + first-step toolchain

- **Base image:** a minimal Debian/Ubuntu (`debian:stable-slim` or `ubuntu:24.04`)
  or, to mirror the existing AL2023 gate, `amazonlinux:2023`. The image must NOT
  pre-install cargo/uv/stack binaries (vanilla requirement, §1). Recommendation:
  **`debian:stable-slim`** for the broad-compatibility default; an `amazonlinux:2023`
  variant can run alongside to reuse the §`al2023-build.yml` glibc-2.34 lesson
  (trusty-analyze's `load-dynamic`/`ORT_DYLIB_PATH` need — DOC env table) if the
  ONNX-backed search index is exercised.
- **First step (STEP 0):** identical to macOS — `curl … sh.rustup.rs | sh` for cargo
  and `curl … astral.sh/uv | sh` for uv, plus `apt-get install -y build-essential
  pkg-config libssl-dev` (the exact system deps `ci.yml` installs). Then STEP 1
  (`cargo install trusty-controller --locked`) onward is platform-identical (DOC-8
  §6: install + ensure are platform-identical; only supervision diverges).

### 5. What the harness asserts (beyond "green")

The oracle (§2.1) is the headline, but the harness asserts a fuller battery so a
"green" stack is green for the right reasons:

- **A0 — hard-dep guide-and-abort (DOC-8 §5).** *Before* STEP 0 provisions the
  toolchain, run a controller invocation needing cargo with cargo absent → ASSERT
  exit `3` and that stderr carries the copy-paste `https://sh.rustup.rs` remediation
  (and the `https://astral.sh/uv` hint when the orchestrator is selected and uv is
  absent). This proves the zero-knowledge user gets actionable guidance, not a stack
  trace.
- **A1 — exit codes (DOC-4 §7 / DOC-5 §4.3).** Each driven command's process exit
  matches its `--json` `exit_code`: `0` ready, `1` down, `2` degraded, `3`
  controller/usage. The harness asserts the *mirror* holds (DOC-4 §8.2 `exit_code`
  field == process exit).
- **A2 — rollup matrix shape (DOC-4 §8.2).** The `stack doctor --json` validates
  against the DOC-4 rollup schema: `verdict ∈ {ready,degraded,pending,down}`,
  `summary` counts sum to the member count (including `na`), every member has
  `cells.{system,project}`, and `clusters[]`/`remediations[]` are well-formed.
- **A3 — version truth post-install AND post-upgrade.** `tctl version --json`
  `stack_version` equals the target tuple, and every member's `<binary> version
  --json .tool_version` equals its BOM pin — asserted **twice** (after STEP 2
  install, and after STEP 5/6 upgrade), so "installed the right versions" and
  "upgrade actually moved them" are both proven.
- **A4 — daemons actually running (DOC-4 §2.0).** Each `kind = "daemon"` member's
  `health --json` envelope `status == "running"` (not merely "binary on disk").
  After an upgrade-restart this is the take-effect proof (DOC-9 §5 / §7).
- **A5 — `.mcp.json` patched.** The harness reads the sample project's `.mcp.json`
  and asserts each enabled two-layer member has its MCP server entry (DOC-8 §2.3
  `patch_mcp_server`), proving the ensure pass wired the project for the
  orchestrator.
- **A6 — idempotency.** Re-running `tctl install` and `tctl ensure` (STEP 4) reports
  no-op and leaves the rollup unchanged — install runs once (DOC-3 §4), ensure is
  safe every launch (DOC-8 §2.3). The harness asserts the second run's reported
  per-member action is "already installed / already ready," not a re-install.
- **A7 — progressive readiness is non-racy (DOC-8 §3.3 / Q3).** The harness uses
  `tctl ensure --wait` so the index is asserted only after it reaches `fresh`. This
  is the explicit answer to the spec's "what does waiting for completion look like?"
  (spec §150) inside an automated gate: the `--wait` flag is DOC-8's
  CI/DOC-10-targeted gate, and DOC-10 is its first consumer. Without `--wait` the
  harness would race the background reindex and flake.
- **A8 — conformance self-check (DOC-6 §8).** Per §2.2, every member passes
  `tctl doctor --self-check`, including the claude-mpm shim advertising exactly
  `["doctor","health","version"]` (DOC-6 §4 / Resolved Decision 3).

### 6. Pinning claude-mpm for the test (resolves the DOC-6 / DOC-9 deferral)

DOC-2 Q6 and DOC-6 §5 (Resolved Decision 7) deferred the **concrete** claude-mpm
version pin to "implementation" — and the *implementation that pins it* is this
harness, because the DOC-6 shim's output-parsing is version-coupled (DOC-6 §4) and
must be tested against a known claude-mpm release. DOC-10 owns the **mechanism**:

- **The harness installs the orchestrator unpinned and captures the resolved
  version.** STEP 2's `tctl install` runs `uv tool install claude-mpm` (DOC-8 §1.4 —
  unpinned in DOC-8, per its Resolved Decision 2) inside the isolated environment.
  The harness then probes the resolved version via the shim
  (`tctl claude-mpm version --json .tool_version`, routed through
  `trusty_common::contract::orchestrator`) and **records it as the tested
  orchestrator version**.
- **That captured version becomes the BOM orchestrator pin the harness asserts
  against.** On the *next* run (and in CI), the harness installs that *exact* pinned
  version (`uv tool install claude-mpm==<captured>`) and the §2.1 oracle asserts
  `tool_version == <captured>` post-install. So the flow is: *first* run discovers
  and freezes the version against which the shim was validated; *subsequent* runs
  assert the frozen pin. When the shim is updated for a newer claude-mpm, the harness
  re-captures and the BOM pin moves in lockstep (DOC-6 §5: "bump it in lockstep with
  shim updates").
- **The captured pin flows into DOC-2's manifest `version` field for the
  orchestrator** (replacing the `0.0.0` placeholder — DOC-2 worked example / Q6).
  This closes the loop: DOC-2 says "pinned at implementation," DOC-6 says "the shim
  is tested against a specific release," and DOC-10 is the place that *picks and
  freezes* that release by running the install in isolation and recording what `uv`
  resolved.

Cross-repo caveat: claude-mpm is an external repo (DOC-6 §4). **If the owner wants a
specific claude-mpm version chosen up front** (rather than "whatever `uv` resolves
the first time the harness runs"), that is an owner decision — see Open Question 3.

#### 6.1 Shim captured-output contract-test (drift fails loudly)

The version pin above protects the *installed bits*; this test protects the *shim's
parsing* against the **input** it consumes. The DOC-6 shim parses claude-mpm's
**human** `mpm-doctor` / version / liveness **text** (not JSON) into the contract
envelope (DOC-6 §4 — the most fragile coupling point, §4.1). So the harness commits a
**golden fixture of the pinned claude-mpm version's raw `mpm-doctor`/version/liveness
output** and asserts the shim against it: feed the captured text in, assert the
synthesized envelope matches. This sits **alongside the existing C3/M2 captured-`--json`
conformance fixtures** of §2.2 — those gate the shim's *output* shape against the
golden schema; this gates the shim's *input* parsing against a frozen sample of what
claude-mpm actually printed at the pinned version.

The payoff is **loud failure on drift**: if a claude-mpm CLI-format change shifts the
`mpm-doctor`/version/liveness text, the captured-output test **fails in CI** rather
than the shim **silently misparsing at runtime** and misclassifying orchestrator
health. Re-capture the fixture whenever the pin advances (lockstep with the shim
update, DOC-6 §5). This is the install-time half of the M3 mitigation; the runtime
half is the shim's loud-degrade rule (unrecognized output → `degraded` with a clear
message, never a confident-but-wrong verdict — DOC-6 §4.1).

### 6b. CI integration vs maintainer-run

The harness must serve **both** the maintainer (MUC1, run-on-demand in a VM) and
CI (regression gate). The constraint is **macOS VM cost in CI** (§3.2). The
recommendation, fitting alongside the existing workflows (`ci.yml`, `release.yml`,
`al2023-build.yml`):

| Leg | Where | When | Rationale |
|---|---|---|---|
| **Linux container** (foreground supervision, §4) | GitHub Actions, `container:` or a `services`-less Ubuntu job (mirrors `al2023-build.yml`/`ci.yml`) | **every PR** (path-filtered to `crates/trusty-controller/**` + the harness + the BOM) | cheap Linux minutes; catches install/upgrade regressions on the **foreground/fallback** branch on every change |
| **systemd-user (per-PR)** (the real Linux v1 product path, §4.1) | a **standard GitHub-hosted ubuntu runner** (a real login session — it has `systemctl --user`, no privileged systemd-in-container or VM needed) | **every PR** (path-filtered, same filter as the container leg) | the real Linux v1 product supervision path (DOC-8 Resolved-Decision-6 is systemd-user) must be gated per-PR, not only on the nightly VM; the foreground-container leg above covers only the fallback branch |
| **macOS smoke (per-PR)** (minimal primary-target gate, §3) | GitHub-hosted `macos-14` runner | **every PR** (path-filtered, same filter as the Linux legs) | macOS is the primary target (spec §172–173) and the cdhash `cp`-into-PATH SIGKILL trap is the nastiest, hardest-to-diagnose failure — both need per-PR signal. A SMALL smoke: `cargo install trusty-controller --locked` → daemon comes up under launchd → `health --json` == `running` → **cdhash assertion** (the on-PATH binary execs without SIGKILL via `cargo install`'s atomic-rename path; the forbidden `cp`-over path is NOT used) → one `restart` (`bootout`→`bootstrap`) → `health` again. NOT the full §2 acceptance scenario (which stays nightly, next row). Costs a few bounded macOS minutes per PR — the trade-off is catching a silent cdhash/supervision regression pre-merge instead of discovering it nightly post-merge. |
| **macOS** (the full §2 MUC2 acceptance run, §3) | GitHub-hosted `macos-14` runner (the runner *is* the isolation host, §3.2) **or** self-hosted tart | **nightly + manual `workflow_dispatch`** (NOT every PR) | macOS minutes are costly; the nightly run is the **full §2 acceptance scenario + teardown** — distinct from the minimal per-PR smoke above — catching the deeper macOS-specific regressions without taxing every PR |
| **Maintainer local** (MUC1) | `tart` VM on the maintainer's Mac; Docker for Linux | **on demand** (a `make` target / script the maintainer invokes) | the spec's MUC1 — test before cutting a stack version, without touching the real machine |
| **systemd-user VM** (optional deeper validation, §4.1) | full Linux VM (lima/cloud), manual | **manual/nightly if shipped in v1** | an **additional** deeper validation atop the per-PR systemd-user-runner leg above; the runner leg already covers the product path on every PR, so the VM leg is now optional (Open Question 2) rather than the sole systemd-user gate |
| **stack-tuple promotion gate** (the §2 acceptance oracle run against a *candidate* tuple) | GitHub Actions, reusing the macOS + Linux legs above | **scheduled (nightly/weekly) + `workflow_dispatch`** | this is the owner/mechanism [DOC-2](./02-stack-manifest-and-versioning.md) §4 references: a candidate tuple (latest-published per crate, or a curated set) is materialized and run through §2; **green promotes it to a released `stack_version`/BOM**. The harness already IS the end-to-end tuple test, so a tuple is "tested" precisely when this run is green — no separate stack-integration matrix is needed. |

Concretely: a new GitHub Actions workflow (e.g. `.github/workflows/isolation.yml`)
with, triggered on PR (path-filtered), a `linux-container` job (foreground/fallback
branch), a `systemd-user-runner` job (on a standard ubuntu runner with
`systemctl --user`, exercising the real Linux product supervision path), and a
`macos-smoke` job (the minimal install + launchd-up + `health` == running + cdhash
assertion + one `restart`); plus a full `macos` job gated behind `schedule:`
(nightly cron) + `workflow_dispatch` (the full §2 acceptance run). This mirrors
`al2023-build.yml`'s "expensive job, path-filtered + `workflow_dispatch`, not every
push" precedent exactly for the deep macOS run, while the bounded per-PR smoke gives
the primary target pre-merge signal. The maintainer entry point is a single invocation (e.g.
`make isolation-macos` / `make isolation-linux`, or
`tctl-isolation-harness --target macos|linux`) that provisions, runs the §2
scenario, asserts the §5 battery, and tears down.

### 7. Harness mechanics

#### 7.1 Which BOM is under test — released stack vs working-tree build

Two modes, both supported; the harness selects via a flag
(`--source released|worktree`):

- **`released` (the default for CI regression + maintainer "is the published stack
  installable?"):** the harness installs from **crates.io** exactly as a real user
  would — `cargo install trusty-controller --locked` pulls the published controller
  (carrying its embedded BOM), and `tctl install` then `cargo install`s each
  published member at its BOM pin. This is the truest UUC2 test (it exercises the
  real install source, including the committed `ui-dist/` bundle so `SKIP_UI_BUILD`
  is unnecessary — DOC-8 §1.3). The **N-1 → N upgrade** (STEP 5) is tested by seeding
  the env at a prior published stack tuple (install controller `@N-1`, then upgrade
  to `N`).
- **`worktree` (for testing an unreleased change before cutting a stack version):**
  the harness `cargo install --path`es the freshly-built binaries from the current
  working tree into the isolated env (still via `cargo install` — atomic, cdhash-safe
  §3.4 — never `cp`). This validates a change *before* it is published. Because the
  worktree may embed an unpublished BOM, the upgrade leg here is N-1(published) →
  N(worktree).

In **both** modes the install into the isolated env goes through `cargo install
--locked` (never a host→VM binary copy), preserving the cdhash invariant (§3.4) and
matching exactly what DOC-8/DOC-9 do in production.

#### 7.2 Provisioning + teardown + reproducibility

- **Provision:** macOS — `tart clone <vanilla-base> <run>` + `tart run` (or, in CI,
  the fresh `macos-14` runner needs no clone — it *is* the clean host); Linux —
  `docker run --rm <base-image>` (the `--rm` gives automatic teardown). The vanilla
  base image (§3.3/§4.2) is version-tagged so runs are reproducible.
- **Teardown:** macOS — `tart stop && tart delete <run>` (disposable clone); Linux —
  the container exits and `--rm` removes it. **Never** touches the maintainer's real
  machine (MUC1): all state (cargo bins, launch agents on the VM, indexes, palaces,
  `.config/trusty-controller`) lives inside the disposable VM/container.
- **Reproducibility:** pin (a) the base image tag, (b) the toolchain version
  (rustup installs the workspace MSRV `1.91`, matching `ci.yml`'s
  `dtolnay/rust-toolchain@1.91`), (c) the stack tuple under test (the BOM
  `stack_version`), and (d) the captured claude-mpm pin (§6). A run is fully
  described by `(base_image_tag, stack_version, source_mode, target_os)`.

#### 7.3 Where the harness lives

Recommendation (see Open Question 4):

- **The scenario driver + assertions live as an `#[ignore]`-tagged integration
  test**, following the **strong in-tree precedent** the grounding found: the
  trusty-search baseline suite (`crates/trusty-search/tests/baseline_trusty_tools.rs`)
  and especially **`crates/trusty-search/tests/bundled_install.rs`** already shell
  out to `cargo install --root <tempdir>` from a Rust `#[ignore]` test and assert on
  the result. DOC-10's driver is the cross-tool generalization of that pattern. It
  belongs in **`crates/trusty-controller/tests/isolation_harness.rs`** (the
  controller crate owns the flow it validates), `#[ignore]`-tagged and run with
  `cargo test -p trusty-controller --test isolation_harness -- --include-ignored`
  (matching the repo convention for slow integration tests, CLAUDE.md).
- **The VM/container provisioning + teardown live as scripts** under
  `crates/trusty-controller/tests/isolation/` (or `scripts/isolation/`): a thin
  `provision-macos.sh` (tart) / `provision-linux.sh` (docker) and the `make`
  targets §6b names. The Rust test asserts; the shell scripts wrap the VM lifecycle
  (Rust is awkward for `tart`/`docker` orchestration; shell is the right tool, and
  the repo already keeps operational scripts in `scripts/`).
- **The CI wiring lives in `.github/workflows/isolation.yml`** (§6b).

So: **assertions in Rust** (reusing the `bundled_install.rs`/baseline precedent and
the `trusty_common::contract` envelope types to parse `--json`), **VM lifecycle in
shell**, **scheduling in a new GitHub workflow** — all net-new (the harness itself
is net-new; only the `cargo install`-in-a-test and daemon-foreground-start patterns
are reused).

---

## Dependencies

### Consumes (inputs)
- **DOC-5** (Accepted) — the CLI entry points the harness drives non-interactively:
  `tctl install`, `tctl ensure [--wait]`, `tctl upgrade`, `tctl updates`,
  `tctl stack doctor`, `tctl stack health`, `tctl version`, `tctl doctor
  --self-check`, with `--yes`/non-TTY behaviour (DOC-5 §3.3) and `--json` machine
  output (DOC-5 §4.2). The harness only ever uses these surfaces — it never reaches
  past the CLI into member internals.
- **DOC-8** (Accepted) — the install/ensure flow under test: the vanilla→ready
  install, the hard-dep guide-and-abort (§5 A0), the macOS cdhash caveat (§3.4), the
  progressive-readiness `--wait` gate (§5 A7), idempotency (§5 A6); and the **Q6
  Linux-supervision deferral** the harness resolves (§4).
- **DOC-9** (Accepted) — the upgrade flow under test: install → upgrade → still-green
  (§2 STEP 5/6), stack-version transitions (N-1 → N), and verify-after (the new
  versions are live — §5 A3/A4); and the **claude-mpm pin deferral** the harness
  resolves (§6).
- **DOC-4** (Accepted) — the rollup `--json` the harness ASSERTS on: verdict
  (`ready|degraded|pending|down`), `exit_code`, the `summary` counts, the
  tools×scope matrix shape, and `clusters[]`/`remediations[]` (§2.1, §5 A1/A2).
- **DOC-2** (Accepted) — the BOM/`stack_version` tuple under test (which versions
  must be live post-install/upgrade — §5 A3) and the manifest the orchestrator pin
  (§6) flows back into.

### Gated by
- **DOC-6** (Accepted) — the conformance layer: the harness can only assert on
  **conformant** members, and reuses DOC-6 §8's `tctl doctor --self-check` as the
  per-member conformance gate (§2.2 / §5 A8). DOC-6 explicitly "gates DOC-10."

### Produces (consumed by)
- **Terminal — nothing depends on the harness.** It is the validation capstone; the
  one thing it *emits back* is the captured claude-mpm pin (§6) that the owner can
  promote into DOC-2's manifest, but that is a one-way data product, not a doc
  dependency edge.

> These edges match the README dependency graph (DOC-10 consumes DOC-5 + DOC-8 +
> DOC-9 + DOC-4 + DOC-2; gated by DOC-6; terminal — nothing downstream).

## Grounding (exists vs. net-new)

Source-first audit, 2026-06-09 (Read + grep against the tree; CI workflows and
existing integration-test patterns confirmed).

| Area | Reality today | Harness implication |
|---|---|---|
| **launchd is per-user** | Root CLAUDE.md + DOC-8 §6: launch agents load into `gui/$(id -u)` (uid-keyed, not `$HOME`-keyed); `trusty_common::launchd` `bootstrap`/`bootout`. | §3: a clean `$HOME` is **not** isolation on macOS — needs a separate uid (VM / fresh runner). |
| **macOS cdhash caveat** | Root CLAUDE.md: never `cp` over an on-PATH binary; `cargo install` atomic-rename is cdhash-safe; a `cp` SIGKILLs the next exec. | §3.4: the harness installs via `cargo install`/`tctl` only, never copies host→VM; it doubles as a regression gate for the property. |
| **Existing CI** | `.github/workflows/{ci.yml,release.yml,al2023-build.yml,line-cap.yml}`. `ci.yml` = fmt/clippy/test/msrv on `ubuntu-latest`, MSRV `dtolnay/rust-toolchain@1.91`, `SKIP_UI_BUILD=1` global. `release.yml` builds on **`macos-14`** (Apple-Silicon) — the project already pays for hosted macOS minutes. `al2023-build.yml` runs in an `amazonlinux:2023` **container**, rustup inside, path-filtered + `workflow_dispatch`. | §6b: a new `isolation.yml` fits the established shape — cheap Linux container job per-PR, expensive macOS job nightly/manual, exactly mirroring `al2023-build.yml`'s "expensive, path-filtered, dispatchable" precedent. `macos-14` runner = the macOS isolation host. |
| **Existing integration-test patterns** | `#[ignore]`-tagged tests shelling out: `bundled_install.rs` runs `cargo install --root <tempdir>` and asserts binaries exist; `baseline_trusty_tools.rs` hits a live daemon started `--foreground &`, discovered via `daemon_url()`. Run with `-- --include-ignored`. | §7.3: the harness driver generalizes `bundled_install.rs` (cargo-install-in-a-test) + the baseline suite (foreground daemon + live `--json` probe). Lives in `crates/trusty-controller/tests/isolation_harness.rs`, `#[ignore]`. |
| **Linux supervision** | DOC-8 §6 / Resolved Decision 6: systemd-user where present, else foreground/daemonless (`<binary> serve`). Daemons support foreground (`trusty-search start --foreground &`, used by the baseline suite). | §4: container harness uses **foreground/daemonless** (no systemd in a stock container); systemd-user validated on a full Linux VM, manual/nightly. Resolves DOC-8 Q6 for the harness. |
| **macOS VM tooling** | **None in-tree** (no Dockerfile, no tart/lima config). `tart`/`lima`/`UTM`/`Anka` are external ecosystem tools. | §3.2: tart recommended (Apple-Silicon native, CI-standard, disposable lifecycle); lima/UTM/Anka as fallbacks; all net-new. |
| **`uv` orchestrator install** | DOC-6 §5: `uv tool install claude-mpm`; version "pinned at implementation". | §6: the harness *is* the implementation that pins it — installs unpinned, captures the resolved version, freezes it as the asserted BOM pin. |
| **The harness itself** | **Fully net-new.** No isolation harness, no VM/container provisioning, no cross-tool install/upgrade acceptance test, no `isolation.yml` exists today. | This document. |

## Cross-cutting notes

- **Isolation-testability (the whole subject).** DOC-10 *is* the realization of the
  isolation-testability cross-cutting requirement flagged in DOC-8 and DOC-9: it
  validates the side-effect-scoping (per-project state keyed on the path slug;
  system state under OS data dirs) and the non-interactive runnability (`--yes`,
  backgrounded reindex, `--wait` gate) that those docs designed *for* this harness.
  If a future change breaks side-effect scoping (e.g. leaks state outside the VM) or
  TTY-only behaviour, the harness fails.
- **Security / no remote code from data.** The harness runs official toolchain
  installers (`sh.rustup.rs`, `astral.sh/uv`) **inside the disposable VM/container
  only** (never on a host), consistent with DOC-8 §5's stance that the *controller*
  does not auto-run them — the harness pre-provisions them as an explicit test step,
  the user/maintainer's deliberate action, not a side effect of `tctl`.
- **macOS cdhash caveat is load-bearing.** §3.4 is not a style note: the harness's
  refusal to `cp` binaries into the VM PATH preserves the very invariant whose
  violation would SIGKILL the stack — and the post-install `health` probe makes the
  harness a live regression gate for it.
- **Zero tool-specific logic (spec §83) flows through.** The harness asserts on the
  generic DOC-4 rollup and DOC-1 envelopes (via `trusty_common::contract` types),
  never on a member's identity, so an orchestrator swap (claude-mpm → trusty-mpm,
  DOC-2 §7 / DOC-6 §6) changes only the BOM entry and the captured pin (§6), not the
  harness assertions.

## Remaining work

- [x] State why isolation is required and what the maintainer-machine anti-pattern is (§1)
- [x] Define the core end-to-end scenario (vanilla → install → ready → ensure →
      upgrade → still-green → verify-after) (§2)
- [x] Define the pass/fail oracle (asserted `--json` fields + exit codes) (§2.1)
- [x] Resolve macOS isolation (launchd-per-user → VM; tart recommended; cdhash
      caveat honoured inside the VM; vanilla base image + first-step toolchain) (§3)
- [x] Resolve Linux isolation + DOC-8 Q6 (container + foreground/daemonless;
      systemd-user on a VM, manual/nightly) (§4)
- [x] Enumerate the full assertion battery beyond "green" (§5)
- [x] Resolve the claude-mpm pin mechanism (capture-and-freeze) (§6)
- [x] Recommend CI-vs-maintainer split (Linux per-PR, macOS nightly/manual) (§6b)
- [x] Specify harness mechanics (released vs worktree BOM; provision/teardown;
      where it lives) (§7)
- [x] **Owner: resolve the decisions** (all 6 approved, 2026-06-09)
- [ ] Team review → ready to implement
- [ ] *(implementation-time)* build `crates/trusty-controller/tests/isolation_harness.rs`
      + `tests/isolation/provision-{macos,linux}.sh` + `make` targets, reusing the
      `bundled_install.rs`/baseline-suite patterns and the `trusty_common::contract`
      `--json` parsers
- [ ] *(implementation-time)* add `.github/workflows/isolation.yml` (linux-container
      per-PR, path-filtered; macos nightly + `workflow_dispatch`), mirroring
      `al2023-build.yml`
- [ ] *(implementation-time / cross-repo)* capture-and-freeze the concrete claude-mpm
      version the shim is validated against (§6) and promote it into DOC-2's manifest
      orchestrator `version` pin

---

## Resolved Decisions

All six decisions were approved by the owner (owner-approved, 2026-06-09).

1. **macOS VM tooling — adopt `tart` as the canonical local tool (§3.2).** *Approved
   as drafted.* The harness uses **tart** (Apple-Silicon-native, CI-standard,
   disposable lifecycle) as the canonical local macOS isolation tool, with lima
   (Linux guests only), UTM (GUI-first), and Anka (commercial) as documented
   fallbacks. In CI, the GitHub-hosted **`macos-14` runner** itself serves as the
   isolation host (no nested VM); the runner's separate login session and uid
   provide the required launchd isolation (§3.2).

2. **Linux legs in v1 — ship foreground/daemonless container leg; systemd-user VM
   is deferred (§4.1).** *Approved as drafted.* v1 ships the **foreground
   container leg** (no systemd, every-PR gate) fully exercising install→ready→upgrade
   in a stock Docker container. The *systemd-user* product path (DOC-8 §6) is
   validated separately on a **full Linux VM** (lima/cloud), run manually/nightly,
   not on every PR. This resolves DOC-8 Q6 for the harness: the container harness
   uses foreground/daemonless supervision because standard Docker has no systemd.

3. **claude-mpm pin — discover-and-freeze, with owner override (§6).** *Approved as
   drafted.* The harness installs claude-mpm **unpinned** the first time
   (`uv tool install claude-mpm`), probes the resolved version via the DOC-6 shim,
   and **freezes that as the asserted BOM orchestrator pin** (flowing back into
   DOC-2's manifest `version`, replacing the `0.0.0` placeholder). The owner may
   override with a named version. Re-capturing occurs when the shim is updated
   (DOC-6 §5 bump-in-lockstep). This satisfies cross-repo dependency on DOC-6.

4. **Harness location — Rust test + shell provisioning + new workflow (§7.3).**
   *Approved as drafted.* Assertions live in
   `crates/trusty-controller/tests/isolation_harness.rs` (`#[ignore]`-tagged,
   generalizing `bundled_install.rs` and the baseline suite); VM/container
   provisioning and teardown in shell scripts under `crates/trusty-controller/tests/isolation/`
   (or `scripts/isolation/`); CI scheduling in a new `.github/workflows/isolation.yml`.

5. **CI cadence — macOS nightly + `workflow_dispatch`; Linux every PR (§6b).**
   *Approved as drafted.* The Linux container leg runs **every PR** (cheap, path-filtered
   to controller + harness + BOM). The macOS leg runs **nightly cron + manual
   `workflow_dispatch`** (costly hosted-macOS minutes), mirroring the established
   `al2023-build.yml` precedent.

6. **Harness source — default `released`; `worktree` opt-in (§7.1).** *Approved as
   drafted.* The harness defaults to **`released`** mode (installs from crates.io,
   testing the real published install path, truest UUC2) for CI regression and
   maintainer usage. The **`worktree` opt-in** (install freshly-built binaries from
   the working tree) is available for pre-release validation before publishing.
