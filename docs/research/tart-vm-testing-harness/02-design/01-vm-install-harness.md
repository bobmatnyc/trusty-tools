# DOC-1 — Tart VM Install Testing Harness

**Status:** Draft — design specification only, **no implementation**
**Source research:** [conclusions-post-measurement.md](../01-research/conclusions-post-measurement.md) (authoritative research outcome), [vm-install-probe-findings.md](../01-research/vm-install-probe-findings.md) (raw measurements A–K)
**Parent design:** [`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`](../../../trusty-installer/research/02-design/10-isolation-testing-harness.md) — **Accepted**, amended by PR #4438 to match these measurements. This document is the concrete, measured, macOS/Tart realisation of that parent design's harness concept.
**Implements:** nothing. This document specifies `vmtest-harness/`; the directory does not exist yet and is not created by this PR.

## Purpose

Specify an **ad-hoc, manually-run installation testing harness** that installs the
trusty-tools stack inside a **clean local Tart macOS VM**, and verifies two things:

1. **Installation succeeds without errors** — every crate installs, every expected
   binary lands on `PATH`, and the machine-readable health surface reports healthy.
2. **Nothing on the host is affected** — no host `~/.cargo`, no host `~/.rustup`,
   no host daemons, no host config, no host `target/`.

The harness is not CI. It is a tool a maintainer runs by hand, on their own Mac,
when they want to answer "does a clean install of this stack actually work today?"
without volunteering their own machine as the test subject.

Every operational rule in this document is forced by a measurement recorded in
[`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md).
Where a claim is an extrapolation rather than a measurement, it is labelled as such
inline. Where something remains unmeasured, it is listed in §13 rather than papered over.

---

## DESIGN

### 1. Settled decisions

These are **decisions, not options**. They were made by the owner on the basis of
the completed research and are recorded here so that the harness implementation
does not relitigate them.

#### D1 — Pattern (a) "released" means crates.io only

Pattern (a) installs via `cargo install <crate> --locked` **from crates.io, and
nothing else**. `install.sh` and prebuilt release tarballs are **out of scope**.

*Rationale:* the crates.io path is grounded in measurement — `cargo install tga
--locked` was executed end-to-end in a guest and timed (131s, 211 deps). The
`install.sh` path was **never** tested end-to-end in a guest. Specifying coverage
for an untested surface would give the harness a claim it cannot support. If
`install.sh` coverage is wanted later, it needs its own measurement pass first
(see §13).

#### D2 — `trusty-mpm` is published, and pattern (a) covers it

**This decision was reversed on 2026-07-31. The original premise was false.**
The superseded text asserted that `trusty-mpm` "carries `publish = false` and is
therefore not on crates.io", and concluded that pattern (a) could not cover it.
Both halves are wrong.

**The corrected fact.** `crates/trusty-mpm/Cargo.toml` contains **no `publish`
key at all**. Its `[package]` table (lines 1–13) declares `name`, `version`,
`edition`, `rust-version`, `license`, `repository`, `description`, `readme`,
`homepage`, `keywords`, `categories`, and `exclude` — and nothing else. Cargo's
default in the absence of that key is `publish = true`. Every textual occurrence
of "publish" in that manifest is a comment *about other crates*, and those
comments say the opposite of the superseded premise: lines 108–111 and 290–291
record that the Tauri GUI is deliberately kept in the separate, `publish = false`
`trusty-mpm-gui` crate precisely so that `trusty-mpm` itself "publishes cleanly to
crates.io". This mismatch was first written up in
[DOC-2 §9.5](./02-harness-contracts.md), which flagged the premise but explicitly
declined to change D2; this amendment closes that open item.

**Verified empirically, 2026-07-31.** `cargo search trusty-mpm --limit 5` returns
`trusty-mpm = "1.0.2"` — the crate exists on crates.io at version **1.0.2**. The
registry is the authority here, not the manifest, and it agrees with the manifest.
(The `crates.io` JSON API was refused for this client under its data-access policy,
so `cargo search` against the registry index is the evidence of record.)

**The consequence.** `trusty-mpm` **is** installable by `cargo install trusty-mpm
--locked` and is therefore **coverable by pattern (a)**. The "documented gap" this
decision used to record **does not exist and is dissolved.** Pattern (a) covers the
full seven-crate stack (D3), and the harness makes the same claim for all three
patterns.

What survives the reversal is the *rule*, not the exception it was invented for:
"released" still means exactly one thing — *what a user gets from crates.io today*
— and a source build must still never be mixed into the released scenario to make a
count match. That rule now costs nothing, because nothing needs excluding.

> **Amendment, 2026-07-31 — D2 reversed.** The original D2 was a settled owner
> decision reached on a premise nobody checked against the manifest: it asserted
> `publish = false` for a crate that has no `publish` key. Everything downstream of
> that premise — the pattern-(a) crate count in D3, the known-absent assertions in
> §7.5, the `expect_a = absent` rows in
> [DOC-2 §9.3](./02-harness-contracts.md) — inherited the error intact. It is
> recorded as a reversal rather than a silent edit because a design whose decisions
> quietly change is a design nobody can audit. The lesson is narrow and worth
> keeping: a claim about publish status is checkable in one command, and this one
> was never run until now.

#### D3 — The stack is seven crates

| # | Crate (workspace directory) | crates.io package | In pattern (a)? |
|---|---|---|---|
| 1 | `trusty-search` | `trusty-search` | yes |
| 2 | `trusty-memory` | `trusty-memory` | yes |
| 3 | `trusty-analyze` | `trusty-analyze` | yes |
| 4 | `trusty-code` | `trusty-code` | yes |
| 5 | `trusty-mpm` | `trusty-mpm` (v1.0.2, verified 2026-07-31) | yes — **amended**, D2 |
| 6 | `trusty-git-analytics` | **`tga`** | yes |
| 7 | `trusty-installer` | `trusty-installer` | yes |

So **all three patterns cover all seven crates.** Row 5 previously read
"— (`publish = false`)" / "**no** — D2"; that is corrected per the D2 reversal
above.

Note the package-name discontinuity on row 6: the workspace directory is
`crates/trusty-git-analytics`, the published package name and the binary are both
`tga`. Scenario and table code must not assume directory name == package name.

#### D4 — Implementation order is (c) → (b) → (a)

Pattern **(c) local-source first**, then (b) branch, then (a) released.

*Rationale:* (c) exercises the one transport unique to this harness — host→guest
source delivery as a `tar` over `tart exec -i` (§5, §6.1) — and **that transport
has never been measured end-to-end**. Patterns (b) and (a) both delegate source
acquisition to the network from inside the guest, reusing machinery they have in
common with any ordinary `cargo install`. Build the unverified thing first.

**What the 112s build does and does not establish.** The 112s `trusty-search`
source build at 8 vCPU/16 GB is a real measurement of *building this workspace from
a source tree inside a guest*, and §9 quotes it as such. It is **not** a
measurement of pattern (c)'s transport. Measurement K3 reached that source tree by
`git clone` **inside the guest** — recorded as `GIT_CLONE_MS=50131` alongside the
build at
[`../01-research/vm-install-probe-findings.md:934-942`](../01-research/vm-install-probe-findings.md).
A guest-side `git clone` is **pattern (b)'s** source delivery, not (c)'s. So the
build cost generalises across (b) and (c) — both build from an on-disk guest tree —
while the delivery step that distinguishes (c) remains untested.

**The tar-over-`tart exec -i` pipeline is UNVERIFIED end-to-end.** What was
measured is a *generic channel property*: 200,000 lines passed through `tart exec`
untruncated
([`../01-research/vm-install-probe-findings.md:179`](../01-research/vm-install-probe-findings.md)),
with exit codes propagating exactly through `-i` (§5.1). That establishes the
channel can carry volume. It does **not** establish the sequence pattern (c)
actually needs: host `git ls-files -co --exclude-standard` → `tar` → `tart exec -i`
→ guest-side unpack → build against the unpacked tree. No such run exists. This is
**devil's-advocate critique #9** — *"tar transfer: SURVIVES host-side, UNTESTED
guest-side"* — recorded at
[`../01-research/devils-advocate-review.md:20`](../01-research/devils-advocate-review.md),
which also lists "transfer of 81 MiB by any mechanism" among the unmeasured items
(`:126-127`). The critique was **not addressed** in earlier drafts of this
document, which claimed the transport had been measured. It had not.

**Why the order still holds — corrected justification.** The (c) → (b) → (a) order
is unchanged, but it follows from the opposite fact to the one previously given.
(c) is built first **because its transport is the unverified one**, and building it
is accepted as the measurement that verifies it. The alternative — write a
standalone tar-transport probe, measure it, then build (c) — was considered and
rejected: the probe would be most of `lib/source.sh` with none of its value, and it
would be a second artifact to keep honest. Implementing (c) first exercises the
transport against the real payload, on the real path, with the oracle already
watching, and the first successful pattern-(c) run becomes the recorded
measurement.

> **Recorded product-owner decision, 2026-07-31.** *Building pattern (c) IS the
> measurement.* The harness accepts an unverified transport as its first
> implementation target rather than running a separate probe first. This is a
> deliberate acceptance of risk, not an oversight: if the tar pipeline does not
> work, it fails loudly during the first (c) implementation, at which point (b) —
> whose transport *was* measured, at `GIT_CLONE_MS=50131` — is the fallback that
> keeps the harness useful. The first successful pattern-(c) run must be recorded
> as the replacement measurement, in the same way §9 asks for the full-stack
> timing.

The interfaces this implementation order needs — the `lib/source.sh` signatures,
the streamed-byte-count logging that turns §6.1's payload estimate into a
measurement, and the scenario composition that keeps (b) available as a fallback —
are specified in [DOC-2 §12](./02-harness-contracts.md), with the transport gap
carried in its open-items list.

#### D5 — Local Tart VM only; dependency download is acceptable

GitHub Actions, Cirrus CI, and any hosted runner are **out of scope**. The harness
runs against a local Tart VM on the maintainer's Mac.

Downloading the full crate dependency graph on every run is **explicitly
acceptable**. There is no registry pre-warming, no vendored registry, no cache
volume. The measured cold-registry cost is already inside the 112s / 131s numbers
in §9, and eliminating it would require either a golden image (rejected, §4.3) or
a host mount (rejected, §6.4).

---

### 2. Placement: `vmtest-harness/`, at the project root, outside the Cargo workspace

The harness lives at repository path **`vmtest-harness/`** — a dedicated
top-level directory, sibling to `crates/`, not nested under it or under any
`scripts/` directory — and is written in **bash**.

#### 2.1 Why not `crates/`

The root `Cargo.toml` declares `members = ["crates/*"]`. That glob is not
advisory — **any** directory created under `crates/` is automatically a workspace
member, and is therefore automatically swept into:

- `cargo test --workspace`
- `clippy --all-targets -- -D warnings`
- the 500-SLOC `.rs` line-cap lint
- the test-pointer lint
- the SLD (spec-linked-docs) lint
- the publish-guard

A harness placed under `crates/` would consequently run on every workspace test
invocation. That directly violates the harness's defining requirement: it is
**ad-hoc and manually run**, and it is **separate from the existing test
infrastructure**. A VM-spawning, multi-minute, host-state-dependent harness that
executes as a side effect of `cargo test --workspace` is a footgun, not a test.
A project-root directory sits even further from the `crates/*` glob than a
nested `scripts/` location would have — the separation argument only gets
stronger the further the harness sits from anything the glob could ever match.

#### 2.2 Why a dedicated top-level directory, not a loose script

The harness is a driver plus a `lib/` of shared modules plus a `scenarios/`
directory plus a data file (§3) — a small program, not a one-off script. Placing
a multi-file harness inside a general-purpose `scripts/` directory buries that
structure among unrelated, single-purpose utility scripts and invites the
`lib/`/`scenarios/` layout to be flattened or picked apart over time. A
dedicated top-level `vmtest-harness/` directory instead signals, on sight, that
this is a self-contained harness with its own internal structure — not a loose
script that happens to be long.

#### 2.3 Why bash is the right encumbrance level

Shell in `vmtest-harness/` is unencumbered by the workspace gates in §2.1: the
line-cap lint is `.rs`-only, and the repo has no shellcheck hook. This holds
regardless of where under the project root the harness sits, root or
`scripts/`, since neither is a workspace member. The harness can therefore be
structured for readability rather than to satisfy Rust lints it has no business
satisfying.

This is a deliberate trade: the harness gives up compile-time checking in exchange
for *not being coupled to the thing it tests*. A Rust harness inside the workspace
would be built by the same toolchain, from the same lockfile, as the code under
test — which is precisely the contamination the harness exists to avoid.

---

### 3. Component architecture

```
vmtest-harness/                      # project root, bash, OUTSIDE the Cargo workspace
├── vmtest                           # driver CLI
├── lib/
│   ├── vm.sh                        # tart lifecycle — the OS boundary for future Linux support
│   ├── provision.sh                 # mise + rust@1.91 + uv + gh  (~30s)
│   ├── source.sh                    # source delivery per pattern
│   └── verify.sh                    # JSON-only assertion oracle
├── scenarios/
│   ├── install-local.sh             # pattern (c)
│   ├── install-branch.sh            # pattern (b)
│   └── install-released.sh          # pattern (a)
└── expected-binaries.tsv
```

#### 3.1 `vmtest` — driver CLI

The single entry point. Responsibilities:

- Parse the scenario selection and run id.
- Run **preflight** (§4.1) and refuse to proceed on any failure.
- Sequence: preflight → clone → size → boot → *negative probe* (§4.2) →
  provision → scenario → verify → teardown.
- Guarantee teardown runs on every exit path, including interrupt and scenario
  failure, via a shell `trap`.
- Emit a final pass/fail with the failing assertion identified.

Suggested surface (illustrative, not normative):

```
vmtest run local|branch|released [--keep] [--runid <id>]
vmtest --check-table                 # expected-binaries.tsv self-diff, no VM (§7.2)
vmtest clean                         # delete orphaned vmtest-* VMs
```

`--keep` skips teardown so a failed run can be inspected. It is the only supported
way to leave a VM behind, and `vmtest clean` is the paired escape hatch.

#### 3.2 `lib/vm.sh` — the OS boundary

**Every** `tart` invocation and every guest-OS assumption lives here, and nowhere
else. This is not tidiness; it is the designed extension seam for future Linux
support (§12.2). Scenarios must never call `tart` directly.

Exports (illustrative): `vm_clone`, `vm_size`, `vm_boot`, `vm_wait_ready`,
`vm_exec`, `vm_exec_stdin`, `vm_wait_for_stopped`, `vm_delete`, `vm_assert_stopped`.

#### 3.3 `lib/provision.sh` — toolchain

Installs `mise`, `rust@1.91`, `uv`, and `gh` in the guest. Measured at **~30s**
(finding K2). Runs *after* the negative probe (§4.2), because the negative probe's
entire value is that the guest has no Rust toolchain yet.

Provisioning must end by **exporting the resolved absolute toolchain paths** — in
particular the mise shims directory — for later steps to self-prefix (§5.2). It
must not rely on having written anything to a shell rc file.

#### 3.4 `lib/source.sh` — source delivery

One function per pattern; see §6. This module owns the host→guest transport and is
the only place that reads the host repository at all.

#### 3.5 `lib/verify.sh` — assertion oracle

JSON-only. See §7. Consumes `expected-binaries.tsv` and the guest's machine-readable
output; produces assertions. It never parses human-readable text.

#### 3.6 `scenarios/*.sh`

A scenario is a **sequence of install steps plus the expectations that follow from
them**. It composes `lib/` functions; it contains no `tart` calls and no transport
logic. This shape is what makes upgrade testing an additional file rather than a
new mechanism (§12.1).

#### 3.7 `expected-binaries.tsv`

The authoritative binary expectation table. See §7.2.

---

### 4. Run lifecycle

#### 4.1 Preflight (host-side, before any VM work)

| Check | Failure mode it prevents |
|---|---|
| `tart` present on `PATH` | obvious |
| Base image `tahoe-base` exists and its digest matches the pinned digest | silent drift of the thing every run is derived from |
| **Every** existing VM the harness would touch is in state `stopped` | §8.1 and §8.2 — a `running` or `suspended` VM is a wedged VM |
| No leftover `vmtest-*` VM with the target runid | name collision mid-run |
| Host has capacity for `--cpu 8 --memory 16384` | see the gap note below |

The **stopped-state refusal** is a hard rule: *refuse to start against any VM not
in `stopped` state.* Do not attempt to stop it, do not attempt to resume it, do not
retry. Both of the failure modes in §8 are unrecoverable-by-retry, and an automated
"fix it up and carry on" path is exactly how a broken image shipped once already.

> **Underspecified in the source research:** host capacity checking was not
> specified. Sizing at 8 vCPU / 16 GB on a host that cannot spare it will degrade
> or fail in ways that look like harness bugs. Recommended: assert available host
> memory before `tart set`, and emit a warning (not a hard failure) if the host
> has ≤8 physical cores.

#### 4.2 The negative probe (before provisioning)

Immediately after the guest becomes reachable and **before** `provision.sh` runs,
assert the **"cargo absent → guide-and-abort"** negative case: with no Rust
toolchain present, the installer path under test must produce its guide-and-abort
behaviour rather than an unhandled error.

This ordering is load-bearing and is one of the two reasons the harness does not
use a golden image (§4.3): a pre-provisioned image has cargo, so this test can
never fire on it. Placing the probe between boot and provisioning is what preserves
it on **every** run.

> **Sequencing note:** the source research states the negative test is "preserved
> on every run" but does not pin *where* in the lifecycle it executes. It is
> specified here at boot-before-provision because that is the only window in which
> the guest genuinely lacks cargo.

#### 4.3 Full sequence

```sh
tart clone tahoe-base vmtest-<runid>      # measured 0.31s (APFS copy-on-write)
tart set vmtest-<runid> --cpu 8 --memory 16384
tart run --no-graphics vmtest-<runid> &
# poll until `tart exec` responds          — measured ~34s first boot
#   (do NOT sleep a fixed interval; poll for the observable ready condition)
<negative probe>                           # §4.2
provision.sh                               # measured ~30s
<scenario>                                 # install-local | install-branch | install-released
verify.sh                                  # §7
wait_for_stopped()                         # NOT a bare `tart stop` — §8.1
tart delete vmtest-<runid>
```

Readiness and shutdown are both **polled for an observable condition**, never
timed. A `tart` command's exit code is not a completion signal (§8.1).

#### 4.4 No golden image — recorded rationale

The harness clones and provisions on every run. It does **not** bake a
pre-provisioned image. This is a decision, and the reasoning is recorded because
the opposite conclusion looks superficially attractive:

- **The saving is negligible.** Baking eliminates only the ~30s provisioning step.
  The clone it would replace costs **0.31s**. Trading a 0.31s operation plus 30s of
  provisioning for a bake pipeline is not a meaningful win against a run whose
  build phase is measured in minutes (§9).
- **It failed three distinct ways in practice.** During research, the golden-image
  approach exhibited: (i) `tart stop` write loss (§8.1); (ii) a golden image that
  **shipped broken as a direct result** of that write loss; and (iii) a
  false-positive bug in the bake script's *own* purity gate — i.e. the mechanism
  meant to certify the image was itself wrong.
- **Clone-and-provision is reproducible from a digest-pinned base.** The
  provenance chain is one pinned digest plus a script in the repo, rather than a
  mutable local artifact whose contents depend on when it was last baked and
  whether that bake shut down cleanly.
- **It preserves the negative test** (§4.2), which a golden image structurally
  destroys.

The superseded bake script is retained in the research directory as
[`../01-research/artifacts/bake-golden.sh.superseded`](../01-research/artifacts/bake-golden.sh.superseded)
for the record. It must not be revived without re-doing the measurements above.

---

### 5. Transport rules

All rules in this section are measured.

#### 5.1 `tart exec` is the sole transport

`tart exec` is the **only** channel between host and guest. Measured properties
that justify this:

- **Exit codes propagate exactly**, including through `-i`. A failing guest command
  fails the harness, with the right code, without wrapper heuristics.
- **stdin heredocs stream**, which is what makes the pattern-(c) `tar` pipe work.
- **stdout and stderr stay separate**, so the JSON oracle can read stdout without
  filtering diagnostics out of it.
- **No truncation at volume** — 200,000 lines passed through intact.

SSH is **not** a transport. It is an opt-in, interactive, post-mortem convenience
for a human inspecting a `--keep` VM. No harness logic may depend on it.

#### 5.2 `tart exec` has no `--env`; self-prefix `PATH` on every command

There is no `--env` flag. Every command the harness sends must set its own `PATH`
explicitly, inline. There is no ambient environment to inherit and no way to
inject one.

#### 5.3 Never depend on guest shell rc files

Two independent measurements force this:

1. **SSH's non-interactive `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`.** Under that
   `PATH`, `mise`, `brew`, and `gh` are simply invisible. Anything that "works when
   I log in and try it" is not evidence that it works under the harness.
2. **A golden image once shipped with `~/.zshenv` missing**, which made `cargo`
   return **127** under *both* `/bin/sh` and `/bin/zsh`. The failure was total and
   silent-looking: a missing dotfile presented as "cargo is not installed."

Consequence: **resolve toolchain paths explicitly** — e.g. the mise shims directory
as an absolute path, captured during provisioning (§3.3) and prefixed onto every
subsequent command. Never `source` an rc file, never assume login-shell semantics,
never rely on `mise activate`.

---

### 6. The three installation patterns

| Pattern | Source delivery | Coverage |
|---|---|---|
| **(c) local** | `tar` of `git ls-files -co --exclude-standard`, piped via `tart exec -i` to guest-local disk | 7 crates; includes uncommitted work |
| **(b) branch** | `git clone` inside the guest (repo is public), checkout branch, `cargo install --path` | 7 crates; committed+pushed state |
| **(a) released** | `cargo install <crate> --locked` from crates.io | 7 crates; latest published state (D2 as amended) |

#### 6.1 Pattern (c) — local source

The host enumerates files with `git ls-files -co --exclude-standard`, streams them
as a `tar` through `tart exec -i`, and unpacks to **guest-local disk**.

Two properties make this the right file set:

- **It includes uncommitted work.** This is the whole point of pattern (c): test
  what is on the maintainer's disk right now, including changes that are not
  committed and not pushed.
- **It excludes `target/` by construction**, because `target/` is gitignored and
  `--exclude-standard` honours that. This is not a hand-maintained exclude list
  that can rot.

Payload size: the working tree is **29 GB**, but the git-tracked content is
**~81 MiB across 5,306 files**. The 29 GB is almost entirely build artifacts that
the file set excludes.

> **Precision note / minor inconsistency in the source research:** the ~81 MiB /
> 5,306 files figure was measured with `git archive`, whereas the specified
> delivery uses `git ls-files -co --exclude-standard`. These are not the same set —
> `-o` adds untracked-but-not-ignored files, which `git archive` never includes.
> The measured figure is therefore a **lower bound / close proxy**, not an exact
> payload size. In practice the delta is whatever untracked non-ignored files the
> maintainer happens to have, which is small in the normal case but unbounded in
> principle. The implementation should log the actual streamed byte count so this
> stops being an estimate.

Build proceeds in the guest against the unpacked tree with `cargo install --path`.

#### 6.2 Pattern (b) — branch

The guest performs `git clone` directly (the repo is public, so no credential
plumbing is needed), checks out the target branch, and runs `cargo install --path`
per crate. No host→guest source transfer occurs; the host repository is not read.

#### 6.3 Pattern (a) — released

`cargo install <crate> --locked` from crates.io, for all seven publishable crates in
D3. `--locked` is mandatory — it is what makes the run reproducible against the
published lockfile rather than against whatever the resolver feels like today.

Remember that `trusty-git-analytics` publishes as **`tga`**: the install command is
`cargo install tga --locked`.

#### 6.4 The host repo is NEVER mounted

Not read-write, and **not read-only**. `--dir` is not used in either direction.

- **Read-write is disqualifying:** guest `cargo` would mutate the host's `target/`
  and, via build scripts and lockfile updates, the host source tree. That converts
  the harness from an isolation tool into a contamination vector — it would be
  strictly worse than testing on the host, because the damage would be invisible.
- **Read-only is also rejected:** it eliminates the write hazard but reintroduces a
  live host dependency into the guest's build inputs, and `--dir` behaviour in
  *either* direction is among the things that were never measured (§13). The
  harness does not depend on unmeasured mechanisms.

Source is **always** copied to guest-local disk. This rule is what closes the one
real hole in the isolation guarantee (§11).

#### 6.5 `tctl install` MUST NOT be used in patterns (b) or (c)

`crates/trusty-installer/src/commands/install.rs`, function `install_one()`, is
**prebuilt-tarball-first with a crates.io `cargo install --locked` fallback**.
There is **no `--path` code path**.

Therefore, invoking `tctl install` during a source-based scenario would fetch a
tarball or a crates.io release and **silently overwrite the source-built binaries
that are under test**. The scenario would then verify a released artifact while
reporting that it verified local source — a false pass, which is the worst possible
harness failure mode.

`tctl install` is legitimate in pattern (a) only insofar as it does exactly what
pattern (a) already specifies; even there, the harness invokes `cargo install`
directly so that all three patterns share one install mechanism and differ only in
source.

---

### 7. Verification — the assertion oracle

#### 7.1 JSON only

The oracle reads **only machine-readable output**:

- `tctl stack doctor --json`
- `tctl version --json`
- daemon health JSON

**Never scrape human-readable text.** Human-readable output is a UI surface: it is
allowed to change wording, add colour, reflow, and localise. An oracle built on it
produces false failures on cosmetic changes and false passes when a message is
reworded around a real regression.

#### 7.2 `expected-binaries.tsv`

The authoritative expectation table. Each row maps an installed crate to the
binaries that install must produce:

| Crate | Expected binaries |
|---|---|
| `trusty-search` | `trusty-search`, `trusty-embedderd` |
| `trusty-memory` | `trusty-memory`, `trusty-bm25-daemon`, `trusty-memory-mcp-bridge` |
| `trusty-installer` | `trusty-installer`, `tctl` |
| `trusty-code` | `tcode` |
| `trusty-analyze` | `trusty-analyze` |
| `tga` | `tga` |
| `trusty-mpm` | `tm`, `trusty-mpm` |

> **Amendment, 2026-07-31 — third `trusty-memory` sidecar added.** The row above
> originally listed two binaries and **omitted `trusty-memory-mcp-bridge`**.
> `crates/trusty-memory/Cargo.toml` declares **three** `[[bin]]` targets:
> `trusty-memory` (`src/main.rs`), `trusty-bm25-daemon` (`src/bin/bm25_daemon.rs`),
> and `trusty-memory-mcp-bridge` (`src/bin/mcp_bridge.rs`) — the last a deprecation
> shim for pre-#914 users — with a manifest comment stating that `cargo install
> trusty-memory` "produces all three binaries in one command". The omission was
> found by enumerating the manifests for
> [DOC-2 §9.3](./02-harness-contracts.md), whose seed table already carries the
> third row; this amendment brings DOC-1 into line with it. The same omission
> exists upstream in the project's Single-Install sidecar inventory checklist
> (`.claude-mpm/INSTRUCTIONS.md`), which is the likely origin of the error and is
> corrected in the same PR.

**Why a table and not the documentation:** `docs/reference/release-workflow.md`
(~lines 450–460) is **stale** with respect to `trusty-console`. Asserting against
that document produces false failures. The TSV is the single authoritative
expectation source for the harness, and the table above is its seed content.

**`--check-table` self-diff mode.** A table checked into the repo will drift from
the crates it describes. `vmtest --check-table` runs on the host, requires no VM,
and diffs the TSV against the workspace's actual declared binaries, reporting
additions, removals, and renames. It is the mechanism that keeps the authoritative
table honest — run it whenever a crate gains or loses a binary.

> **Underspecified in the source research:** the diff *source of truth* for
> `--check-table` was not named. Recommended: the `[[bin]]` targets declared across
> `crates/*/Cargo.toml` (equivalently, `cargo metadata` target kinds), since that
> is the same thing `cargo install` acts on. Deriving it from a second document
> would re-create the staleness problem the table exists to solve.

#### 7.3 Installs go through cargo only — never `cp`

Binaries reach `PATH` **only** via `cargo install`, `cargo install --locked`, or
`cargo install --path`. The harness must never `cp` a binary into a `PATH`
directory.

*Reason: cdhash safety.* Copying a Mach-O binary is not equivalent to installing
it — code-signing identity and the resulting cdhash-dependent behaviour (TCC
attribution, keychain ACLs, notarisation checks) do not survive an arbitrary copy
intact. A harness that `cp`s binaries tests a differently-signed artifact than the
one a user gets.

#### 7.4 Single-Install Convention

Assert that the convention holds: **installing a main crate yields all of that
crate's sidecar binaries.** Concretely — `cargo install trusty-search` must produce
*both* `trusty-search` and `trusty-embedderd`; `cargo install trusty-memory` must
produce **all three** of `trusty-memory`, `trusty-bm25-daemon`, and
`trusty-memory-mcp-bridge`; `cargo install trusty-installer` must produce *both*
`trusty-installer` and `tctl`.

This is a regression gate on packaging: a crate that stops shipping its sidecar
still "installs successfully" and still passes a naive smoke test, but leaves the
user with a stack that cannot start its daemons.

**This gate is only ever as good as §7.2's table.** It cannot detect the loss of a
binary it has never heard of — an omitted row is not a weaker assertion, it is *no*
assertion, and it fails silently and permanently. That is exactly what the
2026-07-31 `trusty-memory-mcp-bridge` omission (§7.2) would have caused: a
Single-Install gate blind to the third sidecar, passing green while the sidecar
disappeared. The `--check-table` self-diff (§7.2) exists to make this class of
omission loud, and it is the reason its diff source must be the `[[bin]]` targets
themselves rather than any prose inventory that can drift.

#### 7.5 Per-pattern expectations

**Amended 2026-07-31 (D2 reversal).** This section previously required `tm` and
`trusty-mpm` to be asserted **known-absent** under pattern (a), because D2 held that
`trusty-mpm` was unpublished. It is published (D2 as amended, v1.0.2), so that
expectation **inverts**: `tm` and `trusty-mpm` are asserted **present** under (a),
(b), and (c) alike, and `tctl stack doctor --json` must report `trusty-mpm` as
installed under every pattern. A pattern-(a) run that does *not* find `tm` is now a
failure, where before it was the expected result.

The oracle is **still pattern-aware by construction** — `expected-binaries.tsv`
carries a per-pattern expectation column (DOC-2 §9.1) and the oracle reads it —
but with the gap dissolved there is currently no in-scope binary whose expectation
differs across patterns. That is a *fact about today's table*, not a licence to
collapse the mechanism: the moment any crate legitimately diverges per pattern, the
column is where that is recorded, and "known-absent" remains an explicit asserted
expectation rather than a skipped assertion.

---

### 8. Operational constraints

Each constraint below is forced by a specific measurement. The evidence is stated
inline so that a future reader can tell the difference between a rule and a
superstition.

#### 8.1 Never bare-`tart stop`

**Rule:** use `wait_for_stopped()` — poll for the observable stopped state. Never
issue a bare `tart stop` and treat its return as completion.

**Evidence:** `tart stop` **silently loses the guest's last write**, reproduced in
**4 of 5 attempts**. It is the **confirmed root cause** of a golden image that
shipped broken (§4.3). See
[`../01-research/logs/k1-tart-stop-asynchrony.log`](../01-research/logs/k1-tart-stop-asynchrony.log),
[`k1b-sync-vs-delay-isolation.log`](../01-research/logs/k1b-sync-vs-delay-isolation.log),
[`k1c-write-loss-repeat-trial.log`](../01-research/logs/k1c-write-loss-repeat-trial.log).

**Generalisation:** *a tart exit code is not a completion signal.* The polling
overhead was itself measured
([`k1d-state-poll-overhead.log`](../01-research/logs/k1d-state-poll-overhead.log))
and is not a meaningful cost.

#### 8.2 Never `tart suspend`

**Rule:** the harness never suspends a VM. Suspended VMs are treated as
unrecoverable and refused at preflight (§4.1).

**Evidence:** resume is **broken and reproducible** — `VZErrorDomain Code=12`.

**Root cause:** `tart list` derives the `suspended` state **purely from the presence
of the `state.vzvmsave` file**, and `tart run` **unconditionally attempts to restore
from it** — which is the step that fails. Retrying therefore can never help; each
retry re-enters the same failing restore.

**Manual unwedge** (for a human, not for the harness):

```sh
mv ~/.tart/vms/<name>/state.vzvmsave{,.bak}
tart run --no-graphics <name>
```

Moving the file aside is what makes `tart list` stop reporting `suspended` and
what makes `tart run` stop trying to restore.

#### 8.3 Preflight refuses non-`stopped` VMs

Covered in §4.1. Restated here because it is the enforcement point for both §8.1
and §8.2: a VM in any state other than `stopped` is either running (so a clone
would be inconsistent) or suspended (so it is wedged per §8.2). Refuse; do not
repair.

#### 8.4 Assert `rustc --version` immediately before each build step

**Rule:** every build step asserts the active `rustc` version immediately before
it builds — not once at provisioning time.

**Evidence:** toolchain drift is **confirmed real** in this repository.
`crates/trusty-git-analytics/rust-toolchain.toml` specifies `channel = "stable"`,
which resolves to rustc **1.97.1** when the current directory is inside that crate,
versus the workspace-pinned **1.91.1** at the repository root. **rustup resolves by
current directory**, so the same command in two directories builds with two
different compilers.

A single provisioning-time version check therefore proves nothing about the
compiler that will actually build a given crate. The assertion must be adjacent to
the build, in the same working directory.

> **Confirmed under a mise-provisioned guest:** provisioning installs `rust@1.91`
> via **mise** (§3.3), and the drift above was measured in-guest on exactly that
> setup. Measurement K5 ran on VM `probe-k2` — the VM provisioned in measurement
> K2 via `mise use -g rust@1.91` — where `rustc --version` and `rustup show` in
> both `crates/trusty-git-analytics/` and the workspace root reproduced the same
> split: rustc **1.97.1** in the crate directory versus **1.91.1** at the root.
> This is expected: mise's rust backend **delegates to rustup** under the hood
> (mise downloads `rustup-init`, and the mise rust entry is a symlink to it — see
> research §E), so rustup's directory-based `rust-toolchain.toml` resolution
> applies normally under a mise-managed toolchain. The per-build-step assertion
> in this rule is justified by this confirmed behaviour, not by uncertainty about
> it.

#### 8.5 Guest sizing is the dominant performance lever

**Rule:** `tart set <vm> --cpu 8 --memory 16384`.

**Evidence:** `trusty-search` — **409 crates** — built in **112s at 8 vCPU/16 GB**,
while `tga` — **211 dependencies**, roughly half the graph — took **131s at 4
vCPU**. Twice the dependency graph, less wall time, because of vCPU count. Sizing
dominates every other tuning knob examined.

#### 8.6 Shared `CARGO_TARGET_DIR`

**Rule:** full-stack runs set a single shared `CARGO_TARGET_DIR` across all crates.

**Rationale:** the seven crates share a large fraction of their dependency graphs.
Per-crate target directories would rebuild those shared dependencies once per
crate. This is the assumption underlying the full-stack extrapolation in §9, and
it is the reason that extrapolation is far below 7 × single-crate time.

#### 8.7 `launchd` works under `tart exec`

**Finding:** `launchctl bootstrap gui/$(id -u)` works under `tart exec` — **no SSH
and no GUI login required**.

**Consequence:** daemon lifecycle and health-endpoint gates are **viable** in this
harness. The oracle can start daemons and assert on their health JSON (§7.1), not
merely assert that binaries exist. This is what makes "installation succeeds" a
meaningful claim rather than a file-existence check.

---

### 9. Measured cost baseline

| Operation | Cost | Conditions |
|---|---|---|
| `trusty-search` build from source | **112s** | 409 crates, 8 vCPU / 16 GB, **cold registry**, thin LTO applied, `ort-sys` ONNX download working in-guest |
| `cargo install tga --locked` | **131s** | 211 deps, **4 vCPU** |
| Provisioning (`mise` + `rust@1.91` + `uv` + `gh`) | **30s** | finding K2 |
| CoW clone (`tart clone`) | **0.31s** | APFS copy-on-write |
| First boot to `tart exec` responding | **~34s** | |
| Subsequent boots | **~18s** | |
| **Full stack** | **~4–8 min** | **EXTRAPOLATION, not a measurement** — see note |

**Full-stack figure — read this before quoting it.** The 4–8 minute range is an
**extrapolation**, and it was extrapolated for **six** crates. D3 puts **seven**
crates in scope for **every** pattern — including (a), which the superseded D2
wrongly scoped at six — so the real figure is somewhat higher than the range says,
in all three patterns rather than just two. No revised number is offered here:
inventing one would be extrapolating from an extrapolation. Two further caveats:

- The extrapolation assumes the shared `CARGO_TARGET_DIR` amortisation of §8.6.
  Without it the number is not close.
- The two per-crate measurements above (112s + 131s for two crates) are not
  obviously consistent with 4–8 minutes for six or seven, *except* under strong
  shared-dependency amortisation. Treat 4–8 min as a **low-confidence planning
  estimate** — and, since the 2026-07-31 D2 amendment widened its scope without
  re-deriving it, **lower-confidence now than when it was written**. It stands only
  until a full-stack run is actually timed. The first pattern-(c) full-stack run
  should be recorded as the replacement measurement.

The ONNX detail matters and is worth keeping explicit: `ort-sys` downloads its
runtime **successfully inside the guest**. Network-dependent build scripts are not
a blocker, which is part of why "download everything, warm nothing" (D5) is an
acceptable posture.

---

### 10. Implementation order

Per D4:

1. **Pattern (c) — local source.** `lib/vm.sh`, `lib/provision.sh`,
   `lib/source.sh` (tar-over-`tart exec -i`), `lib/verify.sh`,
   `scenarios/install-local.sh`, `expected-binaries.tsv`, `--check-table`.
   This delivers the entire skeleton plus the novel transport.
2. **Pattern (b) — branch.** Adds `scenarios/install-branch.sh` and a guest-side
   `git clone` path in `lib/source.sh`. No new infrastructure.
3. **Pattern (a) — released.** Adds `scenarios/install-released.sh` only. No new
   infrastructure. *(Amended 2026-07-31: this step previously also called for
   pattern-aware `trusty-mpm` known-absent handling in the oracle. The D2 reversal
   removes that work item — pattern (a) now expects the same seven crates present
   as (b) and (c), per §7.5.)*

> **Settled:** the architecture sketch annotates `scenarios/install-local.sh` with
> "build first". This means *implement this scenario first*, consistent with D4
> (implementation order (c) → (b) → (a)), and that is the reading adopted above.
> There is no requirement for a separate `cargo build` step preceding `cargo
> install` in the guest. Note, for completeness, that such a step would be
> redundant with `cargo install --path` under a shared `CARGO_TARGET_DIR` (§8.6)
> — it would only add a distinct failure boundary between "compiles" and
> "installs" — but it is not part of the design.

---

### 11. Isolation guarantee

The VM boundary is what makes this harness worth running. The host touchpoints it
covers:

| Host touchpoint | Why it matters |
|---|---|
| `~/.cargo` (registry cache, `bin/`, config) | a host install would overwrite the maintainer's own binaries and mutate their registry state |
| `~/.rustup` (toolchains, default override) | a host run could change the maintainer's default toolchain |
| `~/.local/share/trusty-*` (application state, indexes, databases) | a fresh-install test would destroy or corrupt real working data |
| Running daemons and bound ports | a second `trusty-embedderd` / `trusty-bm25-daemon` would collide with the maintainer's live stack |
| MCP config in `~/.claude` | install steps that register MCP servers would rewrite the maintainer's live agent configuration |
| `~/.trusty-*` (dotfile config) | same |
| Launch agents (`launchctl` registrations) | §8.7 means the harness *does* manipulate launchd — inside the guest. On the host this would install or replace real user agents |

**The only host-side operations the harness performs are `tart` VM lifecycle
commands** — `clone`, `set`, `run`, `exec`, `list`, `delete` — plus reading the
host repository's git-tracked files in pattern (c) (read-only, via `git ls-files`
and `tar`, on the host side of the pipe).

**The never-mount rule (§6.4) is what closes the one real isolation hole.** Every
other touchpoint above is inside the guest filesystem and therefore covered
automatically by the VM boundary. A `--dir` mount would punch a hole straight
through it — read-write catastrophically (guest `cargo` writing host `target/` and
the host source tree), read-only more subtly (host state as a live build input).
Isolation here is not a property of the VM alone; it is a property of the VM *plus*
the discipline of never mounting.

---

### 12. Extension points — design for these, do not build them

#### 12.1 Upgrade testing

A scenario is a **sequence of install steps** (§3.6). An N-1 → N upgrade is
therefore *two install steps in one scenario file* — `scenarios/upgrade-n1-to-n.sh`
— and **not a new mechanism**. No changes to `vm.sh`, `provision.sh`, `source.sh`,
or the oracle are required; the oracle already asserts from a table and JSON, both
of which apply equally after a second install step.

The design obligation today is only this: keep scenarios composed of reusable
install-step functions rather than one monolithic inline script, so that the
sequence is expressible.

#### 12.2 Linux

**All OS-specific behaviour is confined to `lib/vm.sh`** (§3.2). Linux support
means supplying an alternative implementation of that module's exported functions
(Tart supports Linux guests) plus a Linux-appropriate `provision.sh` and a
daemon-supervision path that uses systemd-user rather than launchd (§8.7). The
scenarios, the oracle, and `expected-binaries.tsv` are OS-independent by
construction.

The design obligation today: no scenario, and no part of `verify.sh`, may call
`tart` or assume macOS paths.

---

### 13. Explicit non-goals

- No golden image, no bake script (§4.3).
- No Cirrus CLI and no `.cirrus.yml`.
- No GitHub Actions workflow — CI integration is out of scope (D5).
- No cargo registry pre-warming (D5).
- No `--dir` mounts, in either direction (§6.4).
- No Rust crate, and no `cargo test` integration (§2.1).
- No `install.sh` and no prebuilt-tarball coverage (D1).
- Upgrade testing: deferred (§12.1).
- Linux: deferred (§12.2).

---

### 14. Known gaps — recorded honestly

Things that remain **unmeasured**. None of these are blockers for the design as
specified, but a future change that depends on one of them needs a measurement
first.

- **`install.sh` end-to-end in a guest.** Never tested. This is now out of scope
  per D1, so it is **no longer blocking** — but it also means the harness makes no
  claim whatsoever about the `install.sh` user path.
- **Pattern (c)'s tar-over-`tart exec -i` transport, end-to-end.** Never measured.
  What was measured is a generic channel property — 200,000 lines untruncated
  ([`../01-research/vm-install-probe-findings.md:179`](../01-research/vm-install-probe-findings.md))
  — not the receive-tar → unpack → build sequence pattern (c) depends on. The 112s
  build of measurement K3 reached its source tree by guest-side `git clone`
  (`GIT_CLONE_MS=50131`, `:942`), which is **pattern (b)'s** transport. This is
  devil's-advocate critique #9
  ([`../01-research/devils-advocate-review.md:20`](../01-research/devils-advocate-review.md)),
  and it is **not blocking** only by a **recorded product-owner decision of
  2026-07-31** — not by any technical finding — namely D4's deliberate acceptance of
  risk in treating the first pattern-(c) implementation as the validating
  measurement. Until that run
  succeeds, the harness's headline transport is unproven. Earlier drafts of D4
  wrongly claimed it had been measured; that claim is withdrawn.

- **`--dir` mounts in either direction.** Never measured. The harness does not use
  them (§6.4), and any future proposal to use them must measure first.
- **TCC behaviour under a responsible app other than iTerm2.** **All** TCC
  observations in the research are conditional on having been run **from iTerm2, by
  this user, on this machine**. A LaunchAgent, a cron job, a different terminal
  emulator, or another user account is a **different responsible process** and
  **may prompt**. Do not treat "it did not prompt for me" as a general property of
  the harness.
- **Cirrus CLI.** Nothing about it was measured or evaluated.

**Microphone TCC preflight.** `kTCCServiceAudioCapture` fires **on VM start, even
with `--no-graphics`**. This is a property of **Virtualization.framework**, not of
Tart and not of this harness — the framework performs the check regardless of
whether any audio device is used. Combined with the responsible-app caveat above,
this means the first run in a **new launch context** may present a prompt, and the
harness cannot promise unattended operation in a context that has not previously
been granted.

---

### 15. References

- [`../01-research/conclusions-post-measurement.md`](../01-research/conclusions-post-measurement.md) — **authoritative research outcome**; the settled conclusions this design implements.
- [`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md) — raw measurements **A–K**; every number in §9 and every constraint in §8 traces here.
- [`../01-research/logs/`](../01-research/logs/) — primary logs, including the `tart stop` write-loss trials (K1, K1b, K1c), poll overhead (K1d), provisioning time (K2), and the `trusty-search` build with LTO/ONNX verification (K3, K3b, K3c).
- [`../01-research/README.md`](../01-research/README.md) — research directory index.
- [`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`](../../../trusty-installer/research/02-design/10-isolation-testing-harness.md) — **parent design, Accepted**, amended by **PR #4438** to match these measurements. This document refines DOC-10's harness concept into a concrete macOS/Tart specification; where the two differ in detail, DOC-10 states the requirement and this document states the measured means of meeting it.

> The `01-research/` directory lands in **PR #4456** (branch
> `docs/tart-vm-research-final`). If that PR has not merged, the relative links
> above will not resolve on this branch; they resolve once both branches are on
> `main`.
