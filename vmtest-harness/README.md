# `vmtest-harness/` — install the trusty-tools stack in a clean VM and prove it worked

This harness installs the whole trusty-tools stack inside a **fresh, disposable
macOS VM** and asserts that the install actually succeeded — without touching your
host's cargo registry, toolchains, application state, or running daemons.

It is **ad hoc and manually run**. There is no CI integration, no scheduler, and no
daemon. You run it, it takes 9–16 minutes, it prints a verdict, and it deletes the
VM.

**This file is self-contained.** You do not need to read the design documents to run
it. If you want the *why* behind any rule below, the design set is
[`docs/research/tart-vm-testing-harness/`](../docs/research/tart-vm-testing-harness/)
— DOC-1 is the specification, DOC-2 is the contracts, and `03-plan/MANIFEST.md`
records what every phase of the build actually observed.

---

## Quick start

```sh
# from the repository root
brew install cirruslabs/cli/tart jq        # if you don't have them
tart pull ghcr.io/cirruslabs/macos-tahoe-base:latest
tart clone ghcr.io/cirruslabs/macos-tahoe-base:latest tahoe-base

vmtest-harness/vmtest run local
echo "exit=$?"                              # 0 means everything passed
```

`run local` clones the base image, boots it, provisions a Rust toolchain, streams
your **working tree** into the guest, installs all nine in-scope crates from it,
runs the assertion oracle, and deletes the VM. Expect **9–16 minutes**.

Try `vmtest-harness/vmtest run local --dry-run` first: it runs preflight and prints
the effective configuration, and **creates no VM**. If that exits 0, your host is
ready.

### Host requirements

| Tool | Why | Checked by preflight |
|---|---|---|
| `tart` | the VM backend | yes — exit 10 if missing |
| `jq` | the oracle is JSON-only | yes — present **and functional** |
| `git` | pattern (c) reads your worktree with `git ls-files` | yes |
| `cargo` | host-side version reads | yes |
| `bash` ≥ 3.2 | the stock macOS bash is enough; nothing here needs bash 4 | — |
| a local VM image named `tahoe-base` | what every run clones | yes — and its digest must match `base-image.pin` |
| ≥ 24 GiB RAM | 16 GiB guest + 8 GiB host | yes — **hard fail** |
| ≥ 8 physical cores | `hw.physicalcpu`, not `hw.ncpu` | warn only |

Roughly 100 GiB of free disk is needed for a run; the clone itself is
APFS copy-on-write and costs ~0.3 s, but the guest grows as it builds. It is
**one run at a time** — see [Only one run at a time](#only-one-run-at-a-time)
below — so that figure is not multiplied by anything.

---

## The three subcommands

### `vmtest run local|branch|released [--cpu N] [--memory MIB] [--runid ID] [--keep] [--dry-run]`

The three patterns differ in **one thing only: where the source comes from.** They
share the same provisioning, the same install mechanism (`cargo install` at package
granularity), and the same assertion oracle.

| Pattern | Source | What it is good for |
|---|---|---|
| `local` (c) | your **working tree**, streamed host→guest as a tar over `tart exec -i` — tracked **and** untracked-but-not-ignored files | testing a change you have not committed |
| `branch` (b) | `git clone` of `bobmatnyc/trusty-tools` **inside the guest** — authenticated when you have a `GITHUB_TOKEN`, see [below](#github-credential-propagation-pattern-branch-only) | testing what is actually on a branch, with no host source bytes involved |
| `released` (a) | `cargo install <pkg> --locked` from **crates.io** | testing what a user gets |

Flags:

- `--cpu N` / `--memory MIB` — override guest sizing for this run.
- `--runid ID` — name the run (default: a timestamp). The VM is `vmtest-<ID>`.
- `--keep` — **stop** the guest but do **not** delete it, so you can inspect a
  failure. This is the only supported way to leave a VM behind, and
  `vmtest clean --include-kept` is the paired escape hatch. A kept VM is left
  `stopped`, not running; the driver prints the exact commands to boot and inspect it.
  **Under `run branch` with a token, a kept VM retains your credential on disk** —
  see [the caveat below](#--keep-retains-the-credential-on-disk).
- `--dry-run` — preflight + effective-config banner + acquire/release the run
  registry entry, then **stop before the clone**. No VM is created and no guest is
  touched.

### `vmtest clean [--dry-run] [--include-kept]`

Deletes orphaned `vmtest-*` VMs — ones a crashed or killed run left behind. It
**refuses to touch a running VM** and refuses to touch a VM that a live run still
owns; it prints what it would do and tells you the manual commands instead.
`--dry-run` classifies without destroying. `--include-kept` additionally removes VMs
you deliberately preserved with `--keep` — and for a **stopped** kept VM it
outranks the "a live run owns this" check, because a kept VM plus a leftover
registry entry is exactly what `--keep` produces. It never overrides that check
for a `running` or `suspended` VM.

`clean` never issues a stop. That is deliberate: deciding to stop someone else's VM
is a human's call, not a cleanup tool's.

### Only one run at a time

**The harness runs exactly one guest at a time, and preflight refuses a second
one.** Each guest is sized at 8 vCPU / 16 GiB, and two of them on one host only
contend. Concurrency was a supported mode until 2026-08-04; it is not any more.

Preflight reads this from **the run registry** — the harness's own record of what
is running — not from whether a VM happens to be booted. The two refusals it can
give have **opposite remedies**, and it will tell you which one you have:

| What you see | What it means | What to do |
|---|---|---|
| `another vmtest run is already in progress: … WAIT for that run to finish` | a peer run is live and healthy; it names the runid and PID | **wait.** Do not stop it, do not `vmtest clean` it |
| `harness-namespace VM(s) left behind by a run that is no longer alive … CLEAN IT UP before retrying` | a previous run crashed without tearing down | run **`vmtest clean`** |

If both are true at once you get both reports, and the failure line is the first
one — because waiting is the only thing that is safe while a peer is live.

**It is a mistake-catcher, not a mutex.** Two runs started in the *same instant*
can both get past it, before either has created its registry entry. Nothing else
gets past it, and nothing about the harness depends on that never happening —
but do not build automation on the assumption that the gate serialises for you.

**"A run is in progress" but nothing is running?** Then the registry entry is
stale, and the harness tells you how to clear it. This is a real state, not a
hypothetical: `--keep` leaves its registry entry behind on purpose, macOS reuses
PIDs once the number wraps, and a finished run's PID can end up belonging to
something else entirely.

Each run records **its own command line** when it starts, so a later check can
ask *is that PID still the process that started this run?* rather than guess.
When the answer is a clear no, it says so and carries on:

```
vmtest: WARN: single-run: registry entry 'nightly' was acquired by pid 4711
running `/Users/you/trusty-tools/vmtest-harness/vmtest run local --keep`; that
pid is alive but is now running `/usr/sbin/cupsd`. The pid was REUSED after the
run ended, so the entry is STALE and is being DISREGARDED.
```

When it cannot tell — no `ps`, an unreadable command line, or an entry written
before this check existed — it refuses conservatively and prints both escapes:

```sh
vmtest clean --include-kept          # the supported way
rm -rf ~/.local/state/vmtest-harness/runs/<runid>    # the entry alone
```

`clean --include-kept` reaches both shapes this produces: a **stopped** kept VM
whose entry still answers, and a leftover entry with **no VM at all**. It never
overrides the refusal for a `running` or `suspended` VM.

Be aware of the one edge, which is deliberate: for a **stopped** VM with a `keep`
marker, `--include-kept` deletes it *even if the owning run is still alive*.
That is the point of the flag — `--keep` leaves a stopped VM behind and
`--include-kept` is how you remove one — but it means the flag is not a
"safe unless something is running" switch. Plain `vmtest clean` never does this.
The leftover-**entry** case is the other way round: an entry belonging to a run
the harness can positively confirm is alive is never removed, under any flag.

### `vmtest --check-table`

No VM, no guest, ~1 second. Diffs `expected-binaries.tsv` against the workspace's
actual `[[bin]]` targets read from `cargo metadata`, and reports `ADDED` /
`REMOVED` / `CHANGED` rows. **Run this after any change to a crate's binaries** —
it is how the expectation table is kept from silently rotting.

---

## Exit codes

The code tells you **which phase** failed, so you can tell "my host is not ready"
from "the install is broken" without reading the log.

| Code | Phase | Meaning |
|---|---|---|
| **0** | — | Success. Scenario ran, every assertion passed, teardown completed. |
| **2** | arguments | Usage error — unknown subcommand, bad `--runid`, unknown pattern. **No VM was touched.** |
| **10** | preflight | Host refused: `tart` or `jq` missing, digest mismatch, a VM not `stopped`, runid collision, another run already in progress or a crashed run's VM left behind, insufficient memory. **No VM was created.** |
| **20** | VM lifecycle | `tart clone`/`set`/`run` failed, or boot-ready polling timed out. |
| **30** | negative probe | A precondition probe did not produce its expected result. |
| **40** | provisioning | Toolchain provisioning failed or timed out. |
| **50** | scenario / install | Source delivery failed, or a build / `cargo install` returned non-zero. |
| **60** | verification | Everything installed and an **oracle assertion failed**. This is the interesting failure: the stack installed but is wrong. |
| **70** | teardown | The VM would not stop, or `tart delete` failed. The run's result may have been fine; **the host is not clean.** |
| **130** / **143** | abort | SIGINT / SIGTERM. |

**The first classified failure wins.** A scenario failure (50) followed by a
teardown failure exits **50**, not 70 — you need to know what broke, not what broke
last. Teardown failure is always reported on stderr regardless.

Every harness code is ≤ 70, deliberately below the shell's reserved `126`/`127`/
`128+n` range. That matters here specifically: a golden image once shipped with a
missing `~/.zshenv`, which made `cargo` return **127** and presented as "cargo is
not installed". A harness that also used 127 would have made that harder to read.

---

## Configuration — three tiers, highest wins

1. `vmtest-harness/vmtest.defaults` — the checked-in project defaults. **Never edit
   this to suit one machine.**
2. `VMTEST_<KEY>` environment variables — uppercase the key, prefix `VMTEST_`.
   `VMTEST_CPU=4`, `VMTEST_PROVISION_TIMEOUT=600`, and so on.
3. CLI flags — `--cpu`, `--memory`, `--runid`, `--keep`, `--dry-run`.

`run --dry-run` prints the effective value of every key **with its origin**, so you
can always see which tier won.

Unknown keys in `vmtest.defaults` are an **error**, not a warning. Every timeout in
that file carries either a `file:line` measurement citation or the literal words
`judgment call` — if you change one, keep that property; a tunable whose comment
claims a grounding it does not have is worse than an unlabelled guess.

### The two opt-in modes

Both are **off by default and change nothing when off.** Neither is a CLI flag,
because the five flags above are the whole CLI surface (DOC-2 §8.2); both are
environment variables whose names cannot collide with the `VMTEST_<KEY>` override
mapping, because neither `dirty_check` nor `degraded_check` is a configuration key.
Each must be `0` or `1` — anything else is exit **2**.

| Variable | What the run additionally does |
|---|---|
| `VMTEST_DIRTY_CHECK=1` | Validates pattern (c)'s **defining property** — that the delivered file set includes **uncommitted** work and still excludes **gitignored** paths — by dirtying the host worktree with three sentinel fixtures before the stream and asserting in the guest which of them arrived. |
| `VMTEST_DEGRADED_CHECK=1` | **Fault-injects** the daemon-liveness predicate's non-2xx branch and asserts **both** directions of it. Runs last, after every default assertion has reported. |

```bash
VMTEST_DEGRADED_CHECK=1 vmtest-harness/vmtest run local
```

**`VMTEST_DIRTY_CHECK` is the only thing in the harness that writes to the host
worktree.** It restores its fixtures at the earliest safe point *and* from the
`EXIT` trap, and it refuses to start if those paths are not already clean.

**`VMTEST_DEGRADED_CHECK` writes nothing on the host at all.** Every fault it
injects is inside the guest — two `tctl stop` calls and one `pkill` against guest
processes — and **nothing is restored, because nothing needs to be**: the guest is
deleted by the `EXIT` trap like any other run. It breaks the stack it just
installed, which is exactly why it runs after everything else, and why it is not
the default.

What it asserts, in one run of the **same** predicate the default path uses:

- **Positive** — `trusty-search` is stopped by itself; `trusty-analyze` must then
  answer a **non-200** code with a parseable body and `.status == "degraded"`, and
  the predicate must **accept** it and let the run continue.
- **Negative** — `trusty-review` is SIGKILLed (so its graceful-shutdown `http_addr`
  cleanup never runs and its address stays discoverable) and its launchd job is
  unloaded; the shipped verdict path must then **exit 60** on the
  `NO HTTP RESPONSE (curl code '000')` branch — while still accepting the degraded
  `trusty-analyze` in that same verdict run.

The negative half is the one that matters. A predicate that accepts a 503 is only
correct if it still rejects a daemon that is genuinely gone; without that assertion
all the positive half proves is that the rule got more permissive.

---

## GitHub credential propagation (pattern `branch` only)

`vmtest run branch` clones the repository **inside the guest**. The repository is
public, so that clone has always *worked* anonymously — but anonymously it shares one
github.com rate-limit quota with your host and with every concurrent guest on the same
egress IP. So if the host has a **`GITHUB_TOKEN`** in its environment, the harness
hands it to the guest and the clone is **authenticated**.

| | |
|---|---|
| **Applies to** | `vmtest run branch` **only**. Patterns `local` and `released` never contact github.com and never receive the token — an expired token cannot fail them. |
| **No token set** | **Not an error.** The run clones anonymously and exits 0 exactly as it did before this existed. The log says so once, as information. |
| **Turn it off** | `propagate_github_token` (a `vmtest.defaults` key, default `true`) → `VMTEST_PROPAGATE_GITHUB_TOKEN=false vmtest run branch`. |
| **Bad value** | Anything but exactly `true` or `false` — `0`, `1`, `yes`, `maybe` — is exit **10** at preflight, **before any VM is cloned**, and `--dry-run` catches it too. Values are refused, never guessed at. |
| **Proof it worked** | An in-guest `git ls-remote` against `repo_url` runs as an actual network step. If github.com rejects the token the run fails **40** in a second or two, naming the cause. |

How it gets there, because two things about it are counter-intuitive:

- It is wired in as **`http.https://github.com/.extraheader`**, not as a credential
  helper. A helper is consulted only **after a 401**, and github.com serves a public
  repository with **200** — so `credential.helper store` is never called at all. It
  looks like it works while delivering zero rate-limit relief; a deliberately invalid
  token in the credential store still clones fine. An `extraheader` is sent
  **preemptively** on every request.
- The guest's inherited **interactive** credential-helper chain is **cleared first**
  (`git config --global --replace-all credential.helper ''`). The base image ships a
  `~/.gitconfig` wired to Git Credential Manager; without the reset, writing the header
  fails with `cannot overwrite multiple values with a single value`, and — worse — a
  credential GitHub rejects makes `git ls-remote` **hang** in the headless guest instead
  of failing. The reset turns that hang into an error in a second or two — measured at
  **1.977 s** end to end for the whole credential step on an invalid-token run.

**The token never appears** in `vmtest.defaults`, in the effective-configuration banner,
in `repo_url`, in `$VMTEST_GUEST_ENV`, in host `ps` output, or in `$VMTEST_RUNDIR`. It
crosses to the guest **on stdin** — the same channel the toolchain hand-off uses — into
a **0600** include file created under `umask 077`. Only its presence and the pass/fail
outcome are ever logged.

### `--keep` retains the credential on disk

`--keep` leaves the VM on the host in state `stopped` **with the credential include file
intact**. The header value is **base64, which is encoding and not encryption** — it is
trivially reversible by anyone who can read the VM image or your home directory.

**The harness tells you this at teardown, not only here.** A `--keep` run that actually
propagated a token prints the warning and the include file's path immediately before the
inspection hint, so the caveat arrives when it is actionable rather than only in this
file. A run with no token, and every `local` / `released` run, prints nothing.

- The remedy is **`vmtest clean --include-kept`**, which deletes the kept VM and the file
  with it.
- **If a kept VM was shared, copied, or has outlived its inspection purpose, revoke the
  token.** Deleting the VM afterwards does not undo exposure that already happened.

---

## Two rules you are most likely to break

### 1. `lib/vm.sh` is the ONLY file that may contain the string `tart`

Not the driver, not a scenario, not `verify.sh`. This is the designed extension seam
for a future Linux backend: porting means supplying one alternative implementation
of that module, not auditing the whole harness. It is checked mechanically:

```sh
grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest'
# must list vmtest-harness/lib/vm.sh and nothing else
```

`-w` is required and is not decoration. Without word boundaries the four characters
match inside `started`, which is one of the run registry's mandated filenames, and
the driver shows up in the output on a line that is a *filename* rather than an
invocation. A mechanical check that fires on correct work is worse than no check.

### 2. The host repository is NEVER mounted into the guest, in either direction

Not read-write, not read-only, not "just for the target dir". Pattern (c) delivers
source by **streaming a tar through `tart exec -i`** — a one-way pipe of bytes — and
that is the only host→guest data path in the harness.

Everything else the harness protects (`~/.cargo`, `~/.rustup`, `~/.local/share/
trusty-*`, `~/.claude` MCP config, launchd registrations, bound ports) is inside the
guest filesystem and covered automatically by the VM boundary. **A `--dir` mount is
the one thing that punches straight through it** — read-write catastrophically
(guest `cargo` writing your host `target/` and your source tree), read-only more
subtly (host state becoming a live build input). Isolation here is a property of the
VM *plus* the discipline of never mounting. `--dir` mounts have also **never been
measured** in either direction; any future proposal to use one has to measure first.

Related rules of the same kind, worth knowing before you edit anything:

- **Never a bare `tart stop` trusted as completion.** Shutdown is always
  `vm_request_stop` → `vm_wait_for_stopped` → `vm_delete`. The state flag is not a
  durability flag.
- **Never `tart suspend`.** Resume is broken and reproducibly so; each retry
  re-enters the same failing restore.
- **Teardown runs from an EXIT `trap`,** on every path including interrupt and
  assertion failure — never from the happy path.
- **No harness logic may read the guest's `~/.zshenv`.** It is written for a human
  inspecting a `--keep` VM and nothing else; every harness command self-prefixes its
  own `PATH`. This is drilled deliberately — see "What a green run does not prove".

---

## Rolling the base-image pin forward

`base-image.pin` records the exact OCI digest every run is validated against.
Preflight compares it against `tart list` and exits **10** on a mismatch, printing
both the pinned digest and what it found.

**A pin roll is a deliberate act with its own PR. It is NEVER a repair step inside a
failing run.** If a run fails on a digest mismatch, that is the pin doing its job —
find out why the image changed. An automated "fix it up and carry on" path is how a
broken image shipped once already.

The six steps:

1. Pull the candidate: `tart pull ghcr.io/cirruslabs/macos-tahoe-base:latest`.
2. Record its **full, untruncated** digest
   (`tart list --format json | jq -r '.[] | .Name'` — the OCI row's name is
   `<ref>@sha256:<64 hex>`).
3. Update `base-image.pin`: `digest`, `pinned_on`, `pinned_by`, and a `note` saying
   **why** — a security update, a macOS point release, a tool the guest now needs.
4. **Run all three scenarios green against the candidate before opening the PR.**
   A roll validated against one pattern is **not validated**. The new base is the
   input to every subsequent run.
5. **Re-verify the preinstalled-tool assumptions explicitly.** A new base image is
   precisely where a preinstalled `mise` could move, disappear, or gain a second
   copy — and that class of detail has broken a build before. Provisioning *detects
   and reuses* `mise` and `gh` rather than installing them; if the detection
   assumptions no longer hold, provisioning fails and it should.
6. Open the PR with the before/after digests in the body.

---

## What a green run proves — and what it does not

A `vmtest run <pattern>` exiting 0 proves: the stack **built from that source**,
**all fourteen in-scope binaries landed**, no multi-binary package installed a
partial set, the installed tool versions are internally consistent, and the six
in-scope daemons **answered `/health`**. On a machine with none of your state on it.

It does **not** prove the following, and each of these is a known, recorded gap
rather than an oversight:

- **Daemon health is LIVENESS ONLY.** There is no unified health envelope in the
  product: six daemons, six different `/health` body shapes. The oracle therefore
  asserts only that each one answered, that the body parses as JSON, and that
  `.status` is a string outside `{down, error, unhealthy}`. Its own PASS line says
  `LIVENESS ONLY`. It does not assert that a daemon is *working*.
  *(Tracked as RC-1; open.)*

  **The HTTP status code is logged, not asserted** *(since 2026-08-04)*. It has to
  be: `trusty-analyze` answers **503** with `status:"degraded"` when
  `trusty-search` is unreachable, while `trusty-review` answers **200** with the
  same `status:"degraded"` for the identical condition. The product settled this
  after #4246 — *"the body is the signal; the status code is not"*
  (`probe_http.rs:396-406`) — and the harness had been carrying the rule the
  product fixed. **A daemon that answers nothing at all still fails the run**, as
  does one whose body is not JSON, carries no string `.status`, or reports
  `down`/`error`/`unhealthy`.

  **Both of those claims are now OBSERVED, not inferred** *(2026-08-04)*. They were
  correct-by-construction only until then — on the verifying run `trusty-search`
  was already answering, so `trusty-analyze` returned 200 and the 503 path never
  executed. `VMTEST_DEGRADED_CHECK=1` creates the condition on purpose. In one run
  of one predicate: with `trusty-search` stopped, `trusty-analyze` answered
  **HTTP 503** with `{"status":"degraded",…,"search_reachable":false}` and was
  **accepted**; with `trusty-review` SIGKILLed, the same verdict **exited 60** on
  `NO HTTP RESPONSE (curl code '000')`. The predicate is more permissive about
  status **codes** and no more permissive about **dead daemons**.

  **Three rejection branches remain unexercised** by any run — an unparseable body,
  an absent/empty/non-string `.status`, and `.status ∈ {down, error, unhealthy}`.

  **A squatter emitting a generic `{"status":"ok"}` on a stale port would still
  pass.** That is the **#3364** collision class and it is open. Closing it needs a
  `service` discriminator in each daemon's payload; no daemon emits one, and
  `tctl stack health --json` does not carry one either. RC-1 is what would close it.
- **The guide-and-abort probe (N2) is BLOCKED and reports so on every run.** It was
  meant to prove that `tctl install` refuses helpfully when cargo is absent. It
  cannot be reached that way from a guest: the non-interactive consent gate returns
  **3** before the cargo guard is ever called, and forcing it with `--yes` would
  reach a prebuilt-first path that installs **released** binaries over the
  source-built ones under test — the exact false pass the harness exists to prevent.
  So the run prints `BLOCKED` instead of a green tick it has not earned. The
  underlying exit code and message remain **unpinned by any test**. *(Tracked as
  RC-2; the harness's half is closed as unreachable-by-design, the product's half is
  open.)*
- **`install.sh` is entirely out of scope**, by decision. The harness makes **no
  claim whatsoever** about that user path, and never has.
- **`--dir` mounts were never measured**, in either direction. See rule 2 above.
- ~~**`trusty-analyze` is a daemon the oracle does NOT probe.**~~ **CLOSED
  2026-08-04 — and it was a real gap, not a cosmetic one.** The liveness probe now
  covers **all six** in-scope daemons: `trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-mpm`, `trusty-review`, `trusty-console`.

  **`trusty-console` joined that set on 2026-08-05 with no change to this
  function** *(#4921)*. It was always a `stable_set` daemon; the intersection had
  been excluding it because the expectation table marked it out of scope. Widening
  scope in the TSV was the whole edit — which is the derived set behaving as
  designed rather than a second transcription needing a second fix.

  **The earlier framing here understated it, and the correction is worth stating
  plainly.** It was tempting to call this a logging inaccuracy on the grounds that
  `stack doctor` already covers `trusty-analyze` — doctor *does* report it, and
  `verify_stack_doctor` *does* assert `on_path == true`, a non-null `version`, and
  `plist_installed == false` for it. **But doctor's health predicate accepts
  `down`** for any launchd member with no plist (§1.1a cause (c)), **and `down` is
  exactly what every in-scope daemon reports at the point doctor runs**, because
  nothing has started them yet. So doctor's *health* term was vacuous for
  `trusty-analyze` — as it is for the other four. The difference is that the other
  four were then proven live by the liveness probe and `trusty-analyze` was not.
  **Before this change, nothing in the harness asserted that `trusty-analyze` could
  serve at all.** Its binaries were asserted present, and that was the whole of it.

  The cause was a **hardcoded four-name list** in `verify_daemon_liveness`,
  transcribed from a design table that had omitted `trusty-analyze`. The set is now
  **derived** from `stack doctor`'s own member table (`stable_set()` filtered to
  daemons) intersected with the expectation table, so a stale document cannot
  silently narrow the oracle again, and an empty derived set fails the run instead
  of printing a vacuous PASS.

  This depended on the status-code fix above and could not have shipped without it:
  `trusty-analyze` returns **503** whenever `trusty-search` is not yet answering, so
  adding it under the old 200-only rule would have failed runs that were fine.
- **One host, one user, one terminal.** Every timing figure and every TCC
  observation in this doc set came from one machine, run from iTerm2, by one user.
  Treat the numbers as this machine's, not as the harness's.

### The microphone TCC caveat — read this before automating a run

`kTCCServiceAudioCapture` fires **on VM start, even with `--no-graphics`**. This is a
property of **Virtualization.framework** — not of Tart, and not of this harness. The
framework performs the check regardless of whether any audio device is used.

All TCC observations behind "it does not prompt" were made **from iTerm2, by one
user, on one machine**. A LaunchAgent, a cron job, a different terminal emulator, or
another user account is a **different responsible process and may prompt.**

**The harness cannot promise unattended operation in a launch context that has not
previously been granted.** Do not wire it into something headless and assume silence.

---

## Layout

```
vmtest-harness/
├── vmtest                     # the driver — the single entry point
├── vmtest.defaults            # project defaults; every timeout carries its grounding
├── base-image.pin             # the OCI digest preflight enforces
├── expected-binaries.tsv      # the authoritative binary expectation table
├── lib/
│   ├── vm.sh                  # the OS boundary — the ONLY file that may say `tart`
│   ├── provision.sh           # mise + rust@1.94 + uv + gh, in the guest
│   ├── source.sh              # source delivery, one function per pattern
│   └── verify.sh              # the JSON-only assertion oracle
├── scenarios/
│   ├── install-local.sh       # pattern (c)
│   ├── install-branch.sh      # pattern (b)
│   └── install-released.sh    # pattern (a)
└── tests/
    ├── test-preflight-single-run.sh   # the single-run gate, proved with a stub CLI
    └── dirty-check-fixture.txt        # the dirty-worktree sentinel
```

`tests/test-preflight-single-run.sh` is the one part of the harness you can check
without a VM: `bash vmtest-harness/tests/test-preflight-single-run.sh` takes
**about 15 seconds** (measured 14.7–15.1 s; most of it is deliberate waits for
the live fixture processes it needs), boots nothing, touches no network, and
writes only inside its own temporary directories. Run it after any change to
preflight, the run registry, or `clean`.

A scenario is **a sequence of install steps plus the expectations that follow from
them**. It composes `lib/` functions and contains no `tart` calls, no `PATH`
assignment, no timeout, and no exit code. That shape is why upgrade testing would be
one more file rather than a new mechanism.
