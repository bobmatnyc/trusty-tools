# DOC-9 — Upgrade Flow (UUC3)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted), [DOC-1](./01-tool-contract.md) (Accepted), [DOC-3](./03-scope-model.md) (Accepted), [DOC-5](./05-controller-cli.md) (Accepted)
**Cross-ref:** [DOC-4](./04-doctor-health-rollup.md) (Accepted), [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted), [DOC-7](./07-controller-web-ui.md), [DOC-8](./08-install-bootstrap.md) (Accepted), [DOC-10](./10-isolation-testing-harness.md), [DOC-0](./00-naming-and-doc-charter.md)

## Purpose

Specify cross-tool update detection, changelog-headline rendering, the upgrade
action, and ensuring new versions take effect (restart) — the implementation of
UUC3 (spec §159–164): *the claude-mpm stack is rapidly evolving; the user must
be aware of updates, have an easy path to upgrade, and — once upgraded — the new
versions must take effect.*

This doc owns the **mechanics** of two flows whose **command entry points**
DOC-5 already pinned:

- **`tctl updates`** (and the equivalent `tctl upgrade --check`) — the read-only
  listing: which members have a newer available version, with changelog
  headlines between the installed and target versions. Never mutates.
- **`tctl upgrade [<member>…]`** (alias `update`) — the mutating, **system-scope**
  action: move members to their target versions and **restart** so the new
  version is actually running.

DOC-9 is a **thin coordinator**, exactly like the rest of `tctl` (DOC-5 §2): it
composes *existing, already-grounded primitives* — `trusty_common::update`
(`check_crates_io`, `perform_upgrade`, `upgrade_and_restart`,
`is_launchd_supervised`, `verify_installed_binary`), `trusty_common::launchd`
(`bootout`/`bootstrap`), the graceful-drain `shutdown` (#534), and
`uv tool upgrade` for the orchestrator — per the manifest (DOC-2) and the scope
model (DOC-3). It contains **zero tool-specific logic**: every member's install
source, target version, changelog source, and restart verb come from the
manifest + the contract, never from a compiled-in tool list.

The single net-new value DOC-9 adds over the per-tool `upgrade` command that
already exists on each daemon is **cross-tool orchestration**: one command that
walks the whole BOM, renders aggregated changelog headlines, upgrades each member
in dependency order, restarts every daemon connection-safely, and verifies the
new version is live via the contract — moving the stack as a unit between named
known-good `stack_version` tuples.

---

## DESIGN

### 1. Update detection (`tctl updates`, read-only)

`tctl updates` answers the spec's *"show available updates for all stack tools …
plus changelog headlines for each tool between current and newest available
version"* (spec §90). It is read-only, needs no confirmation, and runs at scope
`all` (it reports, it does not mutate). It is the read-only sibling of
`tctl upgrade --check` (DOC-5 §1.3 — they render the identical diff).

#### 1.1 What "current" and "available" mean

For each `enabled` manifest member the controller computes two version facts:

- **Current (installed) version** — the live version the binary reports. Obtained
  from the contract: `<binary> version --json` → `tool_version` (DOC-1 D3b;
  DOC-2 §6 discovery rule). This is the authoritative installed version — never
  inferred from the manifest. A member that is not installed at all (spawn fails)
  is reported as `not installed` (remediation = `tctl install <member>`, DOC-8).
- **Available (target) version** — the version the controller would move the
  member *to*. There are **two distinct notions** of "newest available," and
  reconciling them is the central detection decision (§1.2).

#### 1.2 Two notions of "available": known-good stack tuple vs crates.io HEAD

The spec phrase "newest available version" is ambiguous in a stack that has both
a curated BOM and independently-published crates (DOC-2 §4). DOC-9 defines both
and picks a safe default:

- **Known-good stack tuple (the default).** The manifest's `[[member]].version`
  pins, labelled by `stack_version` (DOC-2 §1, §4), are the *tested-together*
  target. "Available" = the diff between each member's installed version and its
  BOM-pinned `version`. This is the safe default because the BOM tuple is the only
  combination verified to work together — moving to it is moving to a known-good
  stack version, not a roll of independently-published HEADs.
- **crates.io HEAD (`--latest`, opt-in).** The newest stable version each crate
  has *published*, regardless of whether it has been tested in a tuple. Obtained
  per member via `trusty_common::update::check_crates_io(crate, current)`
  (verified: GETs `https://crates.io/api/v1/crates/<crate>`, parses
  `max_stable_version`, returns `Some(UpdateInfo{current, latest})` only when
  strictly newer). This is the "bleeding edge" view for users who want the freshest
  published bits and accept that the combination is unvalidated.

**Default "available" notion (owner-approved):** `tctl updates` (and `tctl upgrade`)
default to the **known-good stack tuple**; `--latest` switches to the per-crate
crates.io HEAD. Rationale: UUC3's promise is "an easy path to upgrade" for the
average user (spec §25, §162) — that user should land on a tested stack, not a
hand-assembled set of latest crates. `--latest` is the power-user / maintainer
escape hatch and is, by construction, the only path that can move a member *past*
the current BOM (which is what surfaces the "you've drifted off the known-good
tuple" warning in §7).

The two notions compose cleanly because both ultimately feed the same install
primitive (DOC-2 §3): the only difference is which version string the controller
targets — the BOM pin, or the `UpdateInfo.latest` from `check_crates_io`.

#### 1.3 The orchestrator (claude-mpm) detection path

claude-mpm is the one non-cargo, non-crates.io member (DOC-2 §3, DOC-6 §5). Its
detection diverges:

- **Current version** — probed via the DOC-6 shim
  (`trusty_common::contract::orchestrator`), which reports claude-mpm's version
  in the synthesized `version` envelope.
- **Available version** — for the default (stack-tuple) path, the manifest's
  orchestrator `version` pin (DOC-6 §5: "pin at implementation"). For `--latest`,
  the controller queries the Python index that `uv` resolves against
  (`uv tool upgrade --dry-run claude-mpm` style probe, or the package-index JSON);
  **crates.io does not apply** to a Python package. (owner-approved) The actual move is always
  `uv tool upgrade claude-mpm`, never `cargo install`.

#### 1.4 Output: the per-member updates table

`tctl updates` renders a per-member table (rows = manifest members, honoring
`enabled`), each row carrying the installed → target diff and a status:

```
$ tctl updates                                    # default: target = known-good stack tuple

  UPDATES — installed 2026.06-1 → available 2026.07-1
  ─────────────────────────────────────────────────────────────────────────
  MEMBER            CURRENT     AVAILABLE    STATUS
  ─────────────────────────────────────────────────────────────────────────
  trusty-search     0.24.1      0.25.0       ↑ update available
  trusty-memory     0.15.0      0.15.0       ✓ up to date
  trusty-analyze    0.5.1       0.6.0        ↑ update available
  trusty-review     0.3.6       0.3.6        ✓ up to date
  claude-mpm        4.1.2       4.2.0        ↑ update available (uv)
  ─────────────────────────────────────────────────────────────────────────
  3 updates available.  Run `tctl upgrade` to move to stack 2026.07-1.
  ↳ changelog headlines below (between current and available, per tool):
    … (see §2)
```

Read-only listing semantics:

- **No mutation, no confirmation, no restart.** Exit code `0` whether or not
  updates exist (it is a report, not a gate). `--json` (DOC-5 §1.5) emits the
  machine table for DOC-7's UI upgrade indicators and DOC-10 assertions.
- **Per-member resilience.** A member whose version cannot be probed (binary
  missing, hung past `--timeout`) renders `not installed` / `unreachable`
  (reusing DOC-4 §5.1's missing/down/unreachable distinction) — it never aborts
  the listing for the other members.
- **Stack-version header.** The table is framed by the installed stack version →
  the target stack version (§4), so the user reasons in stack-version terms
  ("I'm on `2026.06-1`; `2026.07-1` is available"), not per-crate semver.

`--latest` re-renders the same table with each member's AVAILABLE column sourced
from `check_crates_io` (cargo members) / the Python index (orchestrator), and the
header reads `installed 2026.06-1 → latest (unpinned)` to make the off-tuple
nature explicit.

### 2. Changelog headlines

UUC3 requires the user be *aware of updates* (spec §162); the concrete surface is
"changelog headlines for each tool between current and newest available version"
(spec §90), which DOC-2 §5 grounds in the Keep-a-Changelog format every crate
already writes.

#### 2.1 How headlines are gathered

For each member with an available update, the controller:

1. **Resolves the changelog source** from the manifest `changelog` sub-table
   (DOC-2 §5): `source = "git_tag"` (in-tree/crates.io crates — resolve the
   crate's published `CHANGELOG.md`) or `source = "url"` (claude-mpm — fetch raw
   `https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md`,
   DOC-6 §5).
2. **Parses Keep a Changelog best-effort** (DOC-2 §5): split on H2 version
   anchors `## [<semver>] — <YYYY-MM-DD>` (em-dash or hyphen), select the slice of
   versions `installed_version < v <= target_version`, and within each take the
   **headline** of every list item — the leading bolded summary up to the first
   `—`/sentence break (verified shape: trusty-search writes
   `- **#868 — short summary** — long detail…`).
3. **Renders the bolded leaders only**, grouped by version (newest first), so the
   user sees a scannable "what changed between A and B" without the full prose.

This is exactly the slice DOC-2 §5 specifies; DOC-9 is the consumer that performs
the fetch/parse/render. The fetch is bounded by the same `--timeout` budget as
contract probes (DOC-4 §1.3) so a slow/unreachable changelog source never stalls
the listing.

#### 2.2 Where headlines render

- In **`tctl updates`** — under the per-member table (§1.4), one collapsible block
  per member with an available update.
- In **`tctl upgrade --check`** — identically (it is the dry-run twin of
  `tctl updates`, DOC-5 §1.3).
- In **`tctl upgrade`** (the real action) — printed to **stderr** before the
  blast-radius confirmation prompt (§3.2), so the user reads what they are about
  to install before they confirm.
- In **DOC-7's UI** — from the `--json` rollup (DOC-7 renders, never re-derives).

Rendered example (one member):

```
  trusty-search  0.24.1 → 0.25.0
    [0.25.0]
      • #871 — graceful drain on SIGTERM during reindex
      • #875 — KG expansion now respects skip_kg per-index
    [0.24.2]
      • #869 — fix zero-vector guard on warm reindex
```

#### 2.3 Graceful degradation (best-effort, no CI gate)

Per DOC-2 §5 (owner-approved Resolved Decision 5), headline extraction is
**best-effort with graceful degradation** — there is **no CI lint gate** and a
changelog problem **never fails the upgrade flow**:

- **Changelog missing / unreachable / non-conforming** (no H2 anchor the parser
  recognizes, fetch 404/timeout, malformed) → the controller **skips headlines for
  that one member** and surfaces a soft note
  (`changelog headlines unavailable for trusty-review`), then continues. The
  update is still listed and still upgradable; the user simply loses the preview
  for that tool.
- **No partial-parse failure.** A version slice the parser cannot read contributes
  no headlines rather than aborting; the member's other readable slices still
  render.
- **The upgrade itself is never gated on changelog availability** — headlines are
  informational. A member with a perfect upgrade path and a missing changelog
  upgrades normally with a `(no changelog headlines)` note.

This keeps the cost of changelog drift bounded to "lost preview for one tool,"
exactly as DOC-2 §5 intends.

### 3. The upgrade action (`tctl upgrade`, mutating, system-scope)

`tctl upgrade [<member>…]` is the spec's *"upgrade stack"* (§90) and the
take-effect half of UUC3. It runs at **system scope** (DOC-3 §3: `upgrade` is
system-only — the binary it replaces serves every project/session) and is
**idempotent** (a member already at target is a reported no-op).

#### 3.1 The orchestrated flow

```
tctl upgrade [<member>…]  [--latest] [--yes]            # system scope (forced)
        │
        ▼
1. LOAD MANIFEST            DOC-2 §2 precedence: system override > embedded default
        │                   → members + install descriptor + depends_on + kind + changelog
        ▼
2. SELECT + ORDER MEMBERS   named members, else all enabled (§6 selective vs whole-stack).
        │                   Order by depends_on topologically (search → analyze → review);
        │                   the orchestrator ordered per its depends_on (typically last).
        ▼
3. DETECT (reuse §1)        per member: current (version --json) vs target
        │                   (BOM pin, or check_crates_io/--latest). Already-at-target → no-op.
        ▼
4. RENDER CHANGELOG + WARN  print changelog headlines (§2) to stderr, then the
        │                   blast-radius warning (§3.2). TTY → confirm; --yes bypasses;
        │                   non-TTY without --yes → abort exit 3 (DOC-5 §3.3 / Resolved Q3).
        ▼
5. PER-MEMBER UPGRADE  (in depends_on order; progress on stderr — DOC-5 §4.1)
        │   dispatch on install.source:
        │     • source = "cargo"   → cargo install <crate> --locked   (cdhash-safe; §3.3)
        │                            via trusty_common::update (perform_upgrade)
        │     • source = "python"  → uv tool upgrade <package>        (orchestrator; §3.4)
        │   health-gate the new binary (verify_installed_binary, §3.3)
        ▼
6. MAKE IT TAKE EFFECT      for kind = "daemon": connection-safe graceful restart
        │  (§5)             (launchctl bootout → bootstrap; SIGTERM drain #534) so the
        │                   NEW binary is the running process. Restart AFTER install,
        │                   in dependency order, controller-UI last (§5.2 / DOC-5 §7).
        ▼
7. VERIFY-AFTER (DOC-4)     re-probe each upgraded member: version --json reports the
        │                   target version, health --json → running. Roll up the
        │                   DOC-4 system-track matrix.
        ▼
8. RECORD ACTIVE STACK      on full success, record the new active stack_version (§4.4).
        │
        ▼
9. ROLLUP + EXIT            DOC-4 verdict + exit code; partial failure → continue+report (§6).
```

The flow is manifest-driven end to end: step 5 reads only `install`, step 6 reads
only `kind`, step 7 reads only the contract — no `if member == "trusty-search"`
anywhere.

#### 3.2 Warn-before-system-op (blast radius)

`upgrade` is a system-mutating op, so DOC-3 §5 / DOC-5 §3.3's blast-radius gate
applies mechanically (derived from scope, not per-tool):

```
$ tctl upgrade
  Upgrading to stack 2026.07-1 (3 members: trusty-search, trusty-analyze, claude-mpm).
  ⚠  This replaces system binaries and restarts the affected daemons.
     All active projects and sessions on this machine will be interrupted while
     each daemon bounces.
  Continue? [y/N]
```

- **`--yes` / `-y` bypasses** the prompt (automation/CI/DOC-10).
- **Non-TTY without `--yes` → abort with exit `3`** (DOC-5 §3.3, Resolved Q3 —
  "fail loud"), so a scripted `tctl upgrade` that forgot `--yes` never silently
  bounces every session on a shared box.

#### 3.3 cargo members — install primitive (cdhash-safe)

Cargo members upgrade through the **same primitive** install uses (DOC-8 §8): the
controller composes `cargo install <crate> --locked` via
`trusty_common::update::perform_upgrade` (verified: shells exactly
`cargo install <name> --locked`, inheriting env so `CARGO_HOME`/PATH resolve).
Two CLAUDE.md-grounded subtleties DOC-9 inherits verbatim from DOC-8 §1.3:

- **macOS cdhash / code-signing — never `cp`, always `cargo install`.** `cargo
  install` writes to a temp path and **renames atomically**, keeping the kernel's
  code-signing cache consistent. A plain `cp` over an on-PATH binary can leave a
  stale `cdhash` so the next exec is SIGKILL'd (`EXC_CRASH / CODESIGNING`) before
  any code runs — looks like an OOM but is not. **A re-install that replaces an
  on-PATH binary (which is exactly what upgrade does) is inherently cdhash-safe
  *because* it goes through `cargo install`.** DOC-9 never copies a binary into
  place.
- **`SKIP_UI_BUILD=1` for UI-embedding members** — derived from the manifest
  `ui.available` flag (DOC-8 Resolved Decision 1), set defensively in the install
  subprocess env so a host without pnpm never fails the UI build step on upgrade.

After install, the new binary is **health-gated** via
`trusty_common::update::verify_installed_binary` (verified: probes
`~/.cargo/bin/<bin>` then PATH, runs `<bin> --version` with a 10 s timeout) so the
controller never restarts into a corrupt/incompatible binary; a failed gate keeps
the old binary running and reports the failure (§6).

#### 3.4 The orchestrator — claude-mpm via `uv`

claude-mpm upgrades with **`uv tool upgrade claude-mpm`** (DOC-6 §5,
owner-approved; `uv` is the single tool — no pipx/uvx). `uv` is a preflight hard
dependency (DOC-8 §5): if absent, guide-and-abort with the `https://astral.sh/uv`
hint. Verify-after routes through the DOC-6 shim
(`doctor`/`health`/`version` — the orchestrator's only advertised verbs). The
orchestrator advertises **no `restart` verb** (DOC-6 §4 / DOC-5 §7): it is
user-session-owned, not daemon-supervised, so `tctl upgrade` upgrades the
package but **cannot bounce it** — it notes *"claude-mpm upgraded — restart your
claude-mpm session to apply"* (the "restart between sessions" convention, §5.4).

#### 3.5 Reuse `upgrade_and_restart` vs decompose it

`trusty_common::update::upgrade_and_restart(crate_name, binary_name)` already
composes the full single-tool path: `perform_upgrade` → `verify_installed_binary`
→ (launchd-supervised? `std::process::exit(1)` to trigger KeepAlive respawn :
return a restart hint). DOC-9 reuses this primitive, with one orchestration
nuance:

- `upgrade_and_restart`'s **self-exit** path (`exit(1)`) is designed for a daemon
  upgrading *itself* (the per-tool `trusty-search upgrade` command). The
  **controller is a separate process** upgrading *other* members, so for daemon
  members `tctl` uses `perform_upgrade` + `verify_installed_binary` for the
  install+gate, then drives the **restart explicitly** via the member's `restart`
  contract verb (launchctl `bootout`/`bootstrap`, §5) rather than relying on the
  member self-exiting. This gives the controller deterministic ordering control
  (§5.2) and a verify-after step (§7) that a fire-and-self-exit cannot provide.
- For the **self-upgrade of `tctl` itself**, `upgrade_and_restart`'s supervised
  self-exit path *is* the right model (§8).

So `upgrade_and_restart` is reused directly for self-upgrade and as the reference
implementation for the per-member install+gate; the cross-member restart
sequencing is the net-new orchestration layered on top.

#### 3.6 Ordering

- **Install order = `depends_on` topological** (DOC-2 §3): a dependency is
  upgraded before its dependents (search → analyze → review), so a dependent is
  never running new code against an older-than-expected dependency mid-upgrade.
- **Restart order = same topological order, daemons after their install, the
  controller's own UI service last** (§5.2 / DOC-5 §7) so the controller does not
  kill itself mid-sweep.
- Independent members upgrade/restart in any order (the graph is otherwise free).

### 4. Stack-version transitions

UUC3 is fundamentally about moving a *stack* forward, and DOC-2 §4 defines the
`stack_version` (`YYYY.MM-N`) as the unit the user reasons about. DOC-9 is where a
user actually *moves* between stack versions.

#### 4.1 Stack tuple vs piecemeal latest

- **Move to a known-good stack tuple (default).** The target is the
  `stack_version` + member `version` pins in the *active manifest* (embedded
  default, or a system override that pins a newer tuple — DOC-2 §2). `tctl upgrade`
  walks each member from its installed version to its BOM pin. This is an atomic
  *intent* (move the stack to `2026.07-1`) even though the underlying installs
  happen per member.
- **Piecemeal latest (`--latest`).** Each member moves to its crates.io HEAD
  independently (§1.2). The result is **not** a named stack version — it is an
  ad-hoc tuple. The controller records the active stack as
  `2026.06-1+latest (drifted)` rather than a clean label (§4.4) and warns (§7).

**Default recommendation:** move to the **newest known-good stack tuple**;
`--latest` for the bleeding edge (Open Question 1, consistent with §1.2).

#### 4.2 How a user moves to a newer stack tuple

Two mechanisms, both grounded in DOC-2 §2/§4:

1. **Upgrade `tctl` itself** — the new controller binary carries a newer *embedded*
   BOM, so `cargo install trusty-controller --locked` followed by `tctl upgrade`
   moves the stack to whatever tuple that `tctl` release shipped. This is the
   common path (the controller's release cadence *is* the stack-version cadence).
   See §8 for the self-upgrade mechanics.
2. **Drop a system override `manifest.toml`** (DOC-2 §2) declaring a different
   `stack_version` + pins. `tctl upgrade` then targets that override's tuple. This
   lets a user pin to a specific tuple without rebuilding/reinstalling `tctl`.

#### 4.3 Computing per-member deltas for a target tuple

Given a target manifest (the active one, or an override), the controller computes
the delta set mechanically:

```
for each enabled member m:
    installed = <m.binary> version --json .tool_version      # DOC-1 / §1.1
    target    = m.version                                    # BOM pin (default)
                | check_crates_io(m.crate).latest            # --latest
    if installed < target:  delta = upgrade m: installed → target
    if installed == target: delta = no-op (already at target)
    if installed > target:  delta = refuse downgrade + report drift (owner-approved)
```

The delta set is exactly what §1.4 renders and §3 executes. There is no separate
"stack diff" engine — the stack transition *is* the union of the per-member
deltas, which is why the same detection code (§1) backs both `tctl updates` and
`tctl upgrade`.

#### 4.4 Recording the active stack version

After a successful upgrade the controller records which stack version is now
active, so `tctl status` / `tctl version` / DOC-7 can report "you are on
`2026.07-1`":

- **Clean tuple move** → record the target `stack_version` (e.g. `2026.07-1`) in
  controller state (the system-scope state dir,
  `~/.config/trusty-controller/` per DOC-2 §2 location helpers). (owner-approved)
  Persist the last successfully applied `stack_version` to a small `state.toml`,
  and reconcile against live installed versions on `tctl status` so a manual
  `cargo install` of one member surfaces as drift.
- **`--latest` / partial move** → record a drift marker
  (`2026.06-1+latest` / `2026.06-1 (partial)`), never a clean label, so the user
  always knows whether they are on a tested tuple (§7).

The per-crate version pins themselves are *not* stored by the controller — they
are read live from each binary (DOC-2 §6: the manifest says what *should* be
installed; `version --json` says what *is*). Only the human-facing
`stack_version` label is persisted.

### 5. "New versions take effect" (the explicit UUC3 requirement)

UUC3 states it twice and unambiguously: *"Once upgraded, the new versions of
tools must take effect"* (spec §164). Installing a new binary does **not** make a
running daemon use it — the old process keeps serving until it is restarted.
**The restart/reload step is therefore mandatory, not optional**, and is the
defining responsibility of `tctl upgrade` over a bare `cargo install`.

#### 5.1 Connection-safe graceful restart (daemons)

For each upgraded `kind = "daemon"` member the controller drives a
**connection-safe graceful restart** — the exact convention CLAUDE.md (#534)
mandates and which `trusty_common` already implements:

- **SIGTERM, not SIGKILL.** Use `launchctl bootout` (sends SIGTERM) → `bootstrap`,
  **never** `kickstart -k` (SIGKILL). As of trusty-common 0.10.0 all three HTTP
  daemons implement graceful shutdown via `trusty_common::shutdown::shutdown_signal`
  (verified: awaits SIGTERM/SIGINT, feeds axum `with_graceful_shutdown`), so they
  **drain in-flight requests before exiting**. `mcp_bridge` reconnects with
  exponential backoff across the bounce (CLAUDE.md #534).
- **The primitives are grounded:** `trusty_common::launchd::LaunchdConfig::{bootout,
  bootstrap}` (verified: `bootout` runs `launchctl bootout gui/<uid>/<label>` and
  treats "not loaded" as success; `bootstrap` boots out first then
  `launchctl bootstrap gui/<uid> <plist>`). The controller invokes the member's
  **`restart` contract verb** (DOC-1 lifecycle / DOC-6: composed from `bootout` +
  `bootstrap`), so the per-OS knowledge lives in the member, not the controller.
- **Linux** uses `systemctl --user restart` (or stop+start the foreground process)
  per DOC-8 §6; the deep matrix is DOC-10's. The cdhash caveat is macOS-only.

#### 5.2 Daemons AND UI services AND the controller's own UI

The spec requires restart to cover *"all demonized tools **and UI services**"*
(spec §93). Per DOC-5 §7, the controller derives the restart set from the manifest:

- **Member daemons** (search, memory, analyze) → their `restart` verb. **Each
  member's UI is embedded in its own daemon** (DOC-2 `ui = /ui` on the daemon
  port), so restarting the daemon restarts its UI — there is no separate UI
  process.
- **The controller's own UI service** (DOC-7) → `tctl upgrade` of `tctl` (or an
  upgrade that includes the controller) bounces the controller's own DOC-7 UI
  **last** (DOC-5 §7 / Resolved Q4 — controller-UI restarted last to avoid
  self-kill mid-sweep). For self-upgrade of the controller binary itself, see §8.
- **The orchestrator** (claude-mpm) advertises no `restart` (DOC-6 §4) → skipped
  with the session-restart note (§3.4, §5.4).
- **CLI-only members** (trusty-review) have no long-lived daemon unless in `serve`
  mode → `restart` dispatched if advertised, else `n/a` (DOC-5 §7).

#### 5.3 What the user sees while daemons bounce

The "waiting" UX during a restart is short and explicit (data on stdout, progress
on stderr — DOC-5 §4):

```
  upgrading trusty-search 0.24.1 → 0.25.0 …
    cargo install trusty-search --locked … done
    health gate (trusty-search --version) … ok
    restarting daemon (draining in-flight requests) …
    daemon up at 127.0.0.1:7879 (v0.25.0) … ok
```

During the bounce the daemon is briefly unavailable; because the restart is
graceful (drain-then-exit) and the controller waits for the daemon to answer
`health` again before declaring the member done (§7), the window is the drain
time plus a fast rebind — not an abrupt kill. A still-indexing project after
restart is `project: pending`, **not** an error (DOC-3 §2 / DOC-4 §2.0 — exit 0).

#### 5.4 The "restart between Claude Code sessions" convention

CLAUDE.md prefers restarting daemons *between* Claude Code sessions. DOC-9 honors
this in two ways: (a) the orchestrator (claude-mpm) cannot be bounced by the
controller and is surfaced with an explicit "restart your session" message
(§3.4); (b) the blast-radius warning (§3.2) tells the user that active sessions
will be interrupted, so a user mid-session can choose to defer `tctl upgrade`
until between sessions. The controller never *forces* a session restart — it
restarts the daemons it supervises and informs the user about the session-owned
orchestrator.

### 6. Partial-upgrade / failure handling

Consistent with DOC-8 §7 (install partial-failure, Resolved Decision 7 —
continue+report) and DOC-4's rollup:

- **Continue, don't abort, by default.** A failure on one member (cargo build
  error, network, health-gate fail, failed verify-after) does **not** abort the
  remaining members. The controller records the failure, continues, and reports a
  **DOC-4 system-track matrix** with the failed member as `down` (or
  `contract_incompatible`) plus its remediation.
  - *Dependency exception (DOC-4 §5.4):* if a member's **hard dependency**
    (`depends_on`) failed to upgrade or is now `down`, its dependents are reported
    `blocked-by` the root (one remediation: fix the root) rather than each
    independently failing/restarting.
- **Non-zero exit on any system failure.** `tctl upgrade` exits `1` if any
  member's final system verdict is `down`; `2` if any is `degraded` (e.g.
  older-but-≥-floor contract after upgrade); `0` if all reach target and run.
  A pure project-`pending` after restart is exit `0` (DOC-4 §7).
- **Rollback stance (owner-approved).** `cargo install` is **not transactional**
  and there is **no automatic version rollback in v1.** Rationale: rolling back
  would require the controller to capture and re-install each member's prior
  version — itself a `cargo install <crate>@<old>` that can fail, is not
  cdhash-distinct, and re-introduces a "now I'm on an untested tuple" state.
  Instead DOC-9 relies on three safety properties that make rollback unnecessary
  for the common cases: (1) the **health-gate before restart** (§3.3) means a
  broken *new* binary is caught with the *old* binary still running and serving —
  no restart into a broken process; (2) **idempotent re-run** (below) converges;
  (3) the deterministic recovery path is **upgrade-forward** to a fixed version or
  **explicit downgrade** via `tctl upgrade <member> --version <old>` /
  `cargo install <crate>@<old> --locked` documented as the manual escape hatch.
- **Idempotent re-run is the recovery path.** Because detection is idempotent
  (already-at-target = no-op, §3.1 step 3) and the install primitive is
  re-runnable, the remediation for almost any partial failure is **re-run the same
  `tctl upgrade`** — already-upgraded members are no-ops, only the previously
  failed member is retried (and its restart re-driven). This is the same low-effort
  recovery DOC-8 §7 leans on.
- **Exit codes** mirror DOC-5 §4.3 / DOC-4 §7 (`0` ok · `1` down · `2` degraded ·
  `3` controller/usage error — bad flag, unknown member, non-TTY without `--yes`).

### 7. Selective vs whole-stack upgrade

DOC-5 §1.1 defines both `tctl upgrade` (all enabled members) and
`tctl upgrade <member…>` (named members):

- **`tctl upgrade`** — move every enabled member to the target tuple (the
  whole-stack transition, §4). The clean way to land on a known-good
  `stack_version`.
- **`tctl upgrade <member…>`** — upgrade only the named members. Same flow (§3)
  over a member subset, but ordered to respect `depends_on` for the selected set
  (a selected dependent still waits for a selected dependency).

**Off-tuple drift warning on selective `--latest` upgrade (owner-approved).**
Upgrading a *single* member to the BOM pin keeps the stack within the known-good
tuple (it is just catching that member up). But upgrading one member **off** the
tuple — e.g. `tctl upgrade trusty-search --latest` to a crates.io HEAD newer than
the BOM pin — **drifts** the stack off `stack_version`. DOC-9 **warns** in that
case:

```
  ⚠  Upgrading trusty-search to 0.26.0 (latest) moves it past the known-good
     stack 2026.07-1 pin (0.25.0). Your stack will be marked drifted and is no
     longer a tested combination. Continue? [y/N]
```

and records the active stack as drifted (§4.4). A selective upgrade that only
moves members *up to* their BOM pins does **not** warn (it converges toward the
tuple, not away from it). A *partial* tuple move (only some members to the new
tuple) is also marked drifted until the rest catch up.

### 8. Self-upgrade (upgrading `tctl` / `trusty-controller` itself)

The controller cannot trivially replace its own running binary mid-run (a daemon
serving the DOC-7 UI, or a long-running CLI invocation, is the very process whose
binary is being overwritten). The manifest lists `trusty-controller` as a member
(DOC-2 §3 worked example), so `tctl upgrade` *would* select it — DOC-9 handles it
specially.

**Self-upgrade of `tctl` (owner-approved).** cargo-install + supervised self-exit
(re-exec via supervisor), else message-to-re-run. This reuses
`trusty_common::update::upgrade_and_restart` exactly as designed (its self-exit
path is the *intended* model for a process upgrading itself):

- **Controller running as a supervised daemon** (DOC-7 UI under launchd) →
  `perform_upgrade("trusty-controller")` + `verify_installed_binary("tctl")`, then
  — because the controller is upgrading *itself* — `is_launchd_supervised()` is
  true and the process **self-exits (`exit(1)`)** so launchd's KeepAlive respawns
  the new `tctl` binary. This is the one member where the controller acts on its
  own process; the controller-UI is therefore restarted **last** in a whole-stack
  upgrade (§5.2 / DOC-5 §7) and, for the self-step, by self-exit rather than an
  external bootout (it cannot bootout the process it is currently running in).
- **Controller invoked as a one-shot CLI** (not supervised) → install the new
  binary (cdhash-safe atomic rename, so the on-disk `tctl` is replaced cleanly),
  finish the current command using the *already-loaded* old code, then **print a
  message** to re-run (`trusty-controller upgraded to 0.2.0 — re-run tctl to use
  the new version`). Attempting an in-process re-exec mid-command is unnecessary
  for a CLI and risks confusing partial-state; the atomic install means the *next*
  `tctl` invocation is already the new version.
- **Ordering and inclusion:** in a whole-stack `tctl upgrade`, the controller
  upgrades the *other* members first and the controller itself **last**, so a
  self-exit/respawn never strands an in-flight member upgrade. `tctl upgrade`
  (all) **includes the controller by default, always last**, with an
  **`--exclude-self` escape hatch** to skip self-upgrade if needed.

---

## Dependencies

### Consumes (inputs)
- **DOC-2** (Accepted) — the manifest/BOM the upgrade flow reads: per-member
  `install` descriptor (`source = "cargo"|"python"`, `crate`/`tool`/`package`)
  driving the upgrade command; `version` pins + `stack_version` (the known-good
  target tuple and the unit of transition, §4); `changelog` descriptor (headline
  source, §2); `kind` (which members get a daemon restart); `depends_on` (upgrade
  + restart ordering and failure clustering); `ui.available` (the `SKIP_UI_BUILD`
  signal); and the embedded-default → system-override precedence (how a user pins
  a target tuple).
- **DOC-1** (Accepted) — the contract the controller probes for detection and
  verify-after: `version --json` `tool_version`/`verbs[]`/`contract_version`
  (current version + capability), `health --json` (running after restart),
  `restart` lifecycle verb, exit-code vocabulary (D5), and the older-contract
  degrade rule (D2) applied when an upgraded member still speaks an older contract.
- **DOC-3** (Accepted) — the scope model: `upgrade`/`restart` are **system-scope**
  (blast radius = all projects/sessions), warn-before-system-op, restart ordering
  (system-before-project; you cannot index against a bouncing daemon), and
  project-`pending`-is-not-broken after restart.
- **DOC-5** (Accepted) — the command entry points DOC-9 implements: `tctl updates`
  (read-only listing), `tctl upgrade [<member>…]` (`update` alias, `--check`
  dry-run), the `--yes`/non-TTY abort, the blast-radius confirmation, `--json`
  output, and stdout=data / stderr=progress.

### Produces (consumed by)
- **DOC-10** — the isolation testing harness drives `tctl updates` / `tctl upgrade`
  (and `tctl upgrade --check`) non-interactively (`--yes`) in a vanilla
  container/VM and asserts on the verify-after matrix / exit codes / recorded
  stack version; DOC-9's "runnable non-interactively, take-effect verified via the
  contract" requirement is the contract DOC-10 exercises.

> These edges match the README dependency graph (DOC-9 consumes DOC-1 + DOC-2 +
> DOC-3 + DOC-5; produces into DOC-10).

## Grounding (exists vs. net-new)

Source-first audit, 2026-06-08 (trusty-search MCP search + Read against the tree).

| Area | Reality today | Upgrade-flow implication |
|---|---|---|
| **crates.io version query** | `trusty_common::update::check_crates_io(crate, current)` (verified `update/mod.rs:213`) — GETs `https://crates.io/api/v1/crates/<crate>`, parses `max_stable_version`→`newest_version`→`max_version`, returns `Some(UpdateInfo{crate_name,current,latest})` only when strictly newer; degrades to `None` on any error. `check_throttled` adds a 24 h cache + `CI`/`TRUSTY_NO_UPDATE_CHECK` opt-out. | §1.2 `--latest` detection **reuses** `check_crates_io` directly; the default (BOM-tuple) path reads `version` pins from the manifest and needs no network. |
| **cargo upgrade install** | `trusty_common::update::perform_upgrade(crate)` (verified `update/upgrade.rs:31`) shells `cargo install <crate> --locked`, inherits env, returns `Err` on non-zero exit. Same primitive DOC-8 install uses. | §3.3: upgrade and install compose the **identical** cdhash-safe primitive; the only difference is install runs once vs upgrade moves a pin. |
| **health-gate + restart** | `verify_installed_binary(bin)` (probes `~/.cargo/bin`/PATH, `<bin> --version`, 10 s timeout) and `upgrade_and_restart(crate, bin)` (install → gate → launchd self-exit / hint) verified in `update/upgrade.rs`. | §3.3/§3.5: the gate is **reused** before restart; `upgrade_and_restart` is reused directly for **self-upgrade** (§8) and as the per-member install+gate reference. |
| **launchd supervision detect** | `is_launchd_supervised()` (verified `update/upgrade.rs:142`) — `XPC_SERVICE_NAME` set + no `TERM_PROGRAM`, or PPID==1; macOS-only, else `false`. | §5/§8: drives the self-exit-vs-hint choice for self-upgrade and the supervised-restart path. |
| **connection-safe restart** | `launchd::LaunchdConfig::{bootout,bootstrap}` (verified `launchd.rs:247/281`) — `bootout` = `launchctl bootout gui/<uid>/<label>` ("not loaded" ⇒ Ok; SIGTERM), `bootstrap` = bootout-then-`launchctl bootstrap`. `shutdown::shutdown_signal` (verified `shutdown.rs`) awaits SIGTERM/SIGINT → axum graceful drain; all 3 daemons drain in-flight requests (trusty-common 0.10.0, #534). | §5: the **mandatory take-effect restart** reuses these — SIGTERM drain, never SIGKILL; the controller dispatches the member's `restart` verb composed from them. |
| **per-tool self-upgrade** | Each daemon has its own `upgrade [--check] [--yes]` (verified `trusty-search/src/commands/upgrade.rs`): fresh `check_crates_io`, confirm-unless-`--yes`, delegate to `upgrade_and_restart`. | DOC-9 is the **cross-tool generalization** — one command over the whole BOM, ordered, with aggregated changelog headlines and stack-version transitions. The single-tool path already exists. |
| **uv (orchestrator upgrade)** | DOC-6 §5 (owner-approved): `uv tool upgrade claude-mpm` is the single Python upgrade path; shim reports current version. | §3.4: the one non-cargo upgrade path; `uv` is a preflight hard dep (DOC-8 §5). |
| **Keep-a-Changelog source** | Every crate has a `CHANGELOG.md` declaring Keep a Changelog 1.0.0 with `## [x.y.z] — DATE` H2 anchors and bolded headline list items (verified: trusty-search `CHANGELOG.md` header + `- **#868 — …** — …` entries). | §2: headline extraction **reuses** the existing format (DOC-2 §5 parse contract); net-new is only the fetch/parse/render + graceful degrade, no CI gate. |
| **The cross-tool upgrade orchestration** | **Net-new.** No `tctl`, no cross-tool upgrade loop, no aggregated changelog rendering, no stack-version transition recording exists today. | This document. |

**Net-new (the parts DOC-9 actually adds):** (1) cross-tool orchestration — one
command walking the BOM in `depends_on` order, install+gate+restart+verify per
member; (2) aggregated changelog-headline rendering between installed and target
across tools; (3) stack-version transitions — moving between named known-good
tuples and recording the active stack version (+ drift). Everything else
(crates.io query, cargo install, health-gate, launchd graceful restart, uv
upgrade, the changelog format) is **strongly reusable** and already in the tree.

## Cross-cutting notes

- **Isolation-testability (DOC-10):** the whole flow must be runnable
  **non-interactively** (`--yes` bypasses the blast-radius prompt; non-TTY without
  `--yes` aborts loud) and **verifiable via the contract** (verify-after asserts
  the new `tool_version` is live and `health` is `running`), so DOC-10 can drive
  `tctl upgrade --yes` in a clean VM and assert on the matrix/exit code/recorded
  stack version without contaminating a host.
- **Contract-versioning behavior (upgrade as the remediation for an out-of-date
  contract).** Upgrade is itself the **canonical remediation** DOC-4 §6 routes
  every stale-version / older-contract / below-floor cell to. A member whose
  `contract_version` is below the controller's target (but ≥ floor) renders
  `degraded` (DOC-4 §5.2); a **below-floor / contract-incompatible** member
  (`cv < F`) renders `down` with `reason: "contract_incompatible"` and an
  **upgrade remediation** (DOC-4 §5 / Resolved Q4) — `tctl upgrade <member>` is
  the fix. After a successful upgrade the verify-after step (§7) re-probes
  `contract_version`; if it now meets the floor/target the cell clears, closing the
  loop where "out-of-date contract" → "run upgrade" → "contract now current."
- **Security / no remote code from data:** the controller composes upgrade commands
  from the fixed `source` templates (`cargo install … --locked`,
  `uv tool upgrade …`) — never a free-form command string from the manifest
  (DOC-2 §8). Changelog fetch (`source = "url"`) reads data only; it never executes
  fetched content. Changelog/version output carries no secrets (DOC-1 D8).
- **macOS cdhash caveat is load-bearing:** §3.3's "always `cargo install`, never
  `cp`" applies to upgrade exactly as to install (DOC-8 §1.3) — a re-install that
  replaces an on-PATH binary is cdhash-safe *because* it goes through the atomic
  `cargo install` rename; a `cp` shortcut would SIGKILL the next exec.

## Remaining work

- [x] Specify update detection (`tctl updates`): current via `version --json`,
      available via BOM tuple (default) vs `check_crates_io` (`--latest`),
      per-member table + read-only listing semantics, orchestrator detection (§1)
- [x] Specify changelog-headline gathering/rendering + graceful degradation
      (best-effort, no CI gate) (§2)
- [x] Specify the upgrade action (`tctl upgrade`): warn-before-system-op,
      per-member `cargo install --locked` / `uv tool upgrade`, health-gate,
      take-effect restart, ordering, verify-after, reuse of
      `trusty_common::update` (§3)
- [x] Specify stack-version transitions: tuple vs piecemeal, moving between
      tuples, per-member delta computation, recording the active stack (§4)
- [x] Specify "new versions take effect": mandatory connection-safe restart of
      daemons + UI services + controller UI; SIGTERM drain; restart-between-
      sessions convention; bounce UX (§5)
- [x] Specify partial-upgrade/failure handling: continue+report, rollback stance
      (no auto-rollback; forward-fix/manual downgrade), idempotent re-run, exit
      codes (§6)
- [x] Specify selective vs whole-stack upgrade + off-tuple drift warning (§7)
- [x] Specify self-upgrade of `tctl` (cargo install + supervised self-exit /
      message-to-re-run; controller upgraded last, included by default with
      --exclude-self) (§8)
- [x] **Owner: resolve the open questions → all 7 approved**
- [x] Accepted (owner-approved)
- [ ] *(implementation-time)* build the upgrade/updates orchestration in
      `crates/trusty-controller/src/` (dispatch per manifest; reuse
      `trusty_common::{update, launchd, shutdown, contract}`; the changelog parser)
- [ ] *(DOC-10-owned)* wire `tctl upgrade --yes` + `tctl updates --json` into the
      isolation harness as the upgrade acceptance gate
- [ ] *(DOC-6-owned)* the orchestrator `--latest` probe + pinned claude-mpm version
      flow into the manifest

---

## Resolved Decisions

1. **Default "available" notion — known-good stack tuple vs crates.io HEAD
   (§1.2, §4.1).** (owner-approved) **Default to the manifest's known-good
   `stack_version` tuple**; `--latest` opts into per-crate crates.io HEAD
   (`check_crates_io`) for bleeding-edge. UUC3's "easy path to upgrade" lands
   the average user on a *tested* stack, not an unvalidated assembly of latest
   crates.

2. **`--latest` detection path for the orchestrator (claude-mpm) (§1.3).**
   (owner-approved) Probe the Python index `uv` resolves against (e.g.
   `uv tool upgrade --dry-run claude-mpm` or the package-index JSON) for the
   orchestrator's `--latest` available version; the default (stack-tuple) path
   uses the manifest's orchestrator `version` pin (DOC-6 §5 "pin at
   implementation").

3. **Persisting the active stack version (§4.4).** (owner-approved) Persist the
   last successfully applied `stack_version` to a small system-scope `state.toml`
   (`~/.config/trusty-controller/`), and reconcile it against live installed
   versions on `tctl status` so a manual `cargo install` of one member surfaces
   as drift; record `--latest`/partial moves as a drift marker rather than a
   clean label.

4. **Downgrade handling (§4.3).** (owner-approved) When an installed member is
   **newer** than the target (installed > target), `tctl upgrade` **refuses to
   downgrade and reports drift** by default; an explicit
   `tctl upgrade <member> --version <v>` forces a specific (older) version.

5. **Rollback scope (§6).** (owner-approved) **No automatic version rollback in
   v1.** The health-gate-before-restart (§3.3) prevents restarting into a broken
   binary (the old binary keeps serving), and idempotent re-run + forward-fix /
   explicit manual downgrade (`cargo install <crate>@<old> --locked`) is the
   recovery model.

6. **Off-tuple drift warning on selective `--latest` upgrade (§7).**
   (owner-approved) **Warn + mark the stack drifted** when a selective
   `tctl upgrade <member> --latest` moves a member *past* its known-good BOM pin
   (no longer a tested combination); a selective upgrade that only moves members
   *up to* their BOM pins does not warn. A *partial* tuple move (only some
   members to the new tuple) is also marked drifted until the rest catch up.

7. **Self-upgrade of `tctl` (§8).** (owner-approved) Cargo-install + (if
   launchd-supervised) **self-exit so the supervisor respawns the new binary**,
   else **finish the current command and message-to-re-run**; upgrade the
   controller **last** in a whole-stack sweep. **`tctl upgrade` (all) includes
   the controller by default, always last**, with an **`--exclude-self` escape
   hatch** to skip self-upgrade if needed.
