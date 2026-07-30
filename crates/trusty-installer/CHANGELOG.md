# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- New public `probe_http` module: an HTTP `/health` probe local to trusty-installer, replacing the old CLI-verb probe ([#4246](https://github.com/bobmatnyc/trusty-tools/pull/4246), [#4381](https://github.com/bobmatnyc/trusty-tools/pull/4381)).

### Fixed

- **`tctl install` could SIGKILL-restart healthy daemons mid-flush** ([#4246](https://github.com/bobmatnyc/trusty-tools/issues/4246)). Root cause: the health probe shelled out to `<binary> health --json`, a contract no daemon implements, so healthy daemons falsely reported `down` and drove `needs_kickstart`. Fixed by replacing it with an HTTP `/health` probe local to trusty-installer ([#4381](https://github.com/bobmatnyc/trusty-tools/pull/4381)). Fixes the reported symptom; #4246 stays open pending root-cause confirmation.
- **Follow-up hardening from code review of the above** ([#4388](https://github.com/bobmatnyc/trusty-tools/pull/4388)): the regression-guard tests used a TOCTOU-racy ephemeral port for their "connection refused" fixture, making them latently flaky — replaced with a fixed privileged loopback port that verifies the refusal before returning; a probe test's timeout bounds were inverted (connect budget longer than the total request budget), leaving the `silent`/read-path-timeout case ambiguous — reordered so connect and read-path timeouts are unambiguously distinct; and the `health_string()` / `is_confirmed_down()` asymmetry — a display-only "down" string must never authorise a kickstart — is now documented on both methods and pinned by a new test asserting the confirmed-down set is a strict, non-empty subset of the down-rendering set.

([`bd28002`](https://github.com/bobmatnyc/trusty-tools/commit/bd2800216c2999bbcfb72373a2f81e837b2ab93a), [`6078b21`](https://github.com/bobmatnyc/trusty-tools/commit/6078b21c091b7d4b1ea7d5d05eb96cd49801cfec))

### Changed

- **`detect_does_not_hang_on_an_unresponsive_shadowing_binary` now runs on
  Tokio's paused clock** — 10.01s to 0.00s. The hanging `sleep 30` shadowing
  binary is still really spawned and still really never answers; only the
  health gate's 10s probe timeout moves to the virtual clock. That the child
  is genuinely spawned under a paused clock was verified by measurement (a
  spawn marker written by the fake binary was present on 3 of 3 runs), and a
  new elapsed-time assertion keeps it honest: if `shadowing_version: None`
  ever came from an early probe failure instead of the timeout expiring, the
  test fails rather than passing green on deleted coverage. Reverting the
  probe to the un-timed-out `installed_version` call still fails the test.

---

## [0.4.10] — 2026-07-26

### Fixed

- **Post-install verify could hang indefinitely instead of failing fast when
  a daemon was genuinely dead** (#3875). Root cause: the bounded poll-until-
  ready loop bounded the *number* of health-probe attempts and the aggregate
  wall-clock budget, but each individual `<binary> health --json` subprocess
  call had no timeout of its own — a single hung child (e.g. blocked on a
  half-open socket to a dead daemon) could block `Command::output()` forever,
  defeating every bound above it. Fixed by wrapping every health-probe
  subprocess call in a bounded (10s) timeout that kills and reaps a
  still-running child rather than waiting on it forever; on timeout, the
  probe's stdout/stderr reader threads are also reclaimed within a short
  bounded grace period instead of being left to detach indefinitely.
- **Verify table reported genuinely-installed binaries as `not_installed`
  under an incomplete `PATH`** (#3876). Root cause: binary presence was
  resolved via `which::which` alone, which depends entirely on `PATH`;
  binaries installed to `~/.local/bin` were mis-reported `not_installed`
  whenever that directory was missing from `PATH` (see #3874). Fixed by
  falling back to probing the default install directory directly when
  `which` fails, so the verdict is resilient to a broken `PATH`.
- **`install.sh` only added `~/.local/bin` to `PATH` via `.zshrc`, which
  non-interactive and non-login zsh invocations (e.g. a plain
  `ssh host 'cmd'`) never source** (#3874), causing false "not found"
  preflight warnings and bogus verify results on fresh machines. Fixed by
  writing the PATH export to `.zshenv`, which zsh sources unconditionally for
  every invocation type (interactive, login, non-interactive, script) — so
  one write covers the gap without also writing `.zprofile`/`.zshrc`, which
  would make a normal login+interactive session (a fresh Terminal.app window
  sources all three) triple the install dir in `PATH` on a single fresh
  install. bash gets both `.bashrc` and `.bash_profile` (its interactive and
  login files respectively — a default bash session sources only one of the
  two, not both). Each write skips itself only when the file already
  contains a genuine, live `PATH=` assignment (not a commented-out line, and
  not a same-prefix sibling like `MANPATH=`/`FPATH=`) naming the install dir
  as a `:`/quote/whitespace-delimited path segment (not merely as a
  substring of a longer, different directory) — recognising any quoting
  style, not only this script's own — so repeat installs, or a machine with
  a pre-existing hand-written PATH entry, never get a duplicate export line,
  while a commented-out or unrelated-variable entry never silently blocks
  the real PATH export from being written.

- **`tctl install -y` dead-ended on a truly clean macOS box with no
  Homebrew: tmux could never be auto-installed, and the install ran to
  completion anyway before failing with an unexplained exit 2** (#3821,
  demo-critical — reproduced piping `install.sh | sh -s -- -y` on a fresh
  macOS 15.7.7 arm64 VM). Root cause: Phase 6's prereq-check correctly
  detects that no auto-install command is safe when no package manager is
  present (bootstrapping Homebrew itself needs sudo/CLT and can prompt, so
  it is intentionally never auto-run), but the caller then only warned and
  continued installing the rest of the stable set — the actual failure
  only surfaced later, at the post-install verify tail, with no message
  connecting it back to tmux. Fixed with a new pure decision gate
  (`commands::tmux_gap::decide_tmux_gap_action`, mirroring
  `install_gate::decide_install_gate`): under `--yes` with tmux still
  missing after Phase 6, `tctl install` now fails EARLY — before installing
  anything else — with one clear, actionable message (both human and
  `--json` output). Brew-present machines are unaffected: `brew install
  tmux` still auto-installs under `-y` exactly as before. Interactive TTY
  behavior is unchanged (prompts to continue anyway). Issue #3823 (tm
  needs a tmux *server* started) is separate and out of scope here.
- The Phase 6 prereq auto-install command (`brew install tmux` and
  friends) now runs fully captured (never `Stdio::inherit()`, #3830) via a
  new `prereqs::exec_heartbeat::run_captured_with_heartbeat` — removing a
  latent footgun where a future reordering of the prereq phase relative to
  the live install checklist could reintroduce #3830's interleaved-line
  corruption.
- **A first-run `brew install tmux` can legitimately take 1-5+ minutes
  (Homebrew self-update, a non-bottled build) with zero output** — code
  critic caught this on PR #3879: the fully-captured exec above would sit
  silently for that entire span on the exact piped `curl | sh -s -- -y`
  demo path, reading as a hang. `run_captured_with_heartbeat` now emits an
  installer-owned `still running: ... (Ns elapsed)...` line every ~15s
  while the child is still running — never child stdout passthrough, so
  captured output discipline is unaffected.
- **`tctl install --json`'s Phase-6 fail-early error dropped the real cause
  when a package manager WAS present but the auto-install attempt itself
  failed** (network/disk/permissions) — the JSON envelope carried only the
  static "Homebrew required" hint, indistinguishable from "no package
  manager found at all". The envelope now carries `hint` (the static,
  platform-appropriate note) and `detail` (the actual captured stderr from
  the failed attempt, `null` when no attempt was even possible) as
  distinct fields.

## [0.4.9] - 2026-07-24

### Fixed

- **The 0.4.8 defensive service-bootstrap postcondition never ran on the
  already-installed / re-run path — exactly the machines it exists to repair**
  (#3841, demo-critical; reproduced on a real demo MacBook minutes after
  0.4.8 shipped). Root cause: `service_bootstrap::bootstrap_one` checked
  `plist_present(binary)` FIRST and returned `Skipped` immediately whenever a
  plist already existed on disk — before ever reaching the #3836 `is_loaded`
  → `bootstrap_fallback` postcondition below it. A plist left behind by an
  EARLIER, pre-0.4.8 run that hit #3832 (trusty-memory's `service install`
  wrote the plist without loading it) is exactly "already present"; a 0.4.8
  re-run over that state took the skip branch and the defensive fallback
  never fired, leaving `com.trusty.memory` permanently absent from
  `launchctl list` and `tctl install` exiting 2 / `NOT VERIFIED`. Fixed by
  running the same `is_loaded` → `bootstrap_fallback` postcondition on the
  already-present branch too (reported as the new
  `BootstrapAction::LoadedByFallback`), never re-running `service install`
  over an existing plist (the non-clobber guarantee is unchanged — only the
  *load* is repaired, not the plist itself).
- Belt-and-braces second layer: the post-install verify tail
  (`verify_tail::verify_one`) now also attempts the installer's own
  `launchctl bootstrap` fallback for any member it classifies
  `DownState::NotLoaded` whose plist is present on disk, re-probing on
  success (#3841). This means a future regression in the install-time
  skip-path handling — or any member the install-time bootstrap didn't run
  for this invocation — still self-heals at verify time instead of silently
  reporting `NOT VERIFIED`. Code-critic fix round on PR #3849 (APPROVE, 2
  MEDIUM): the post-fallback re-probe now goes through the SAME bounded
  `poll_until_not_down`/`bounded_attempts` machinery the #2498 kickstart
  retry uses (respecting whatever of the aggregate poll budget the member's
  own kickstart phase already spent) instead of a single instant re-probe
  that could race a just-repaired but slow-starting daemon into a false
  `NOT VERIFIED` — the same race class #3833 fixed for the kickstart path;
  and the fallback attempt itself (`apply_not_loaded_fallback`) now takes an
  injectable `ServiceEnv` (mirroring `bootstrap_one`), with new unit tests
  covering both a successful and a failed fallback outcome.
- Manual workaround for a machine already stuck in this state (installer
  0.4.8, no code fix applied): `launchctl bootstrap gui/$(id -u)
  ~/Library/LaunchAgents/com.trusty.memory.plist && launchctl kickstart -k
  gui/$(id -u)/com.trusty.memory`.
- Simplified the macOS Full-Disk-Access / App-Data TCC guidance `tctl install`
  prints after `trusty-search` / `trusty-mpm` land (#3846). Three defects
  fixed: (1) the guidance now detects whether a binary already sat at the
  install path before this run and prints a first-install variant (grant
  once, nothing to remove) instead of always showing reinstall-only
  remove-then-re-add steps; (2) the preamble no longer says "Every
  `cargo install`..." — it is installation-method-agnostic, matching the
  prebuilt-tarball install path `tctl` actually uses; (3) each component's
  guidance is now ≤3 lines and points at
  `docs/reference/release-workflow.md` for full detail, and the
  `TRUSTY_SIGN_IDENTITY`/`tctl sign` tip prints once per install run instead
  of once per component. Code-critic follow-up (same PR): the fresh-vs-reinstall
  detection and the guidance's interpolated binary path now come from the
  CONCRETE path `install_one` actually placed the binary at, rather than a
  fixed `install_dir` guess — the guess named the wrong file/directory and
  picked the wrong guidance variant whenever the `cargo install` fallback
  (as opposed to the prebuilt-tarball path) placed the binary.

## [0.4.8] — 2026-07-24

### Fixed

- `tctl install`'s live per-component progress checklist (#3804) no longer
  spams duplicate/interleaved lines on a real terminal (#3830, demo-critical).
  Root cause: `RealServiceEnv::run_service_install` shelled out to
  `<binary> service install` via `Command::status()`, which INHERITS the
  parent's stdout/stderr — so that child's output wrote directly into the
  terminal region `LiveChecklist`'s `indicatif::MultiProgress` was actively
  redrawing (other components' rows keep steady-ticking in the background
  for the whole install loop), desyncing indicatif's own "how many lines did
  I just draw" tracking and producing exactly the observed duplicate,
  interleaved output. Fixed by switching that call to `Command::output()`,
  which captures the child's stdout/stderr instead of inheriting them; any
  failure's stderr is now folded into the returned error message rather than
  silently dropped. Reproduced and verified via a PTY capture replayed
  through a VT100 terminal emulator (before: dozens of duplicated
  `installed` lines; after: a clean, stable N-row block).
- Second occurrence of the same #3830 bug class, found by code-critic on
  PR #3834: the `cargo install` fallback path (`install_one`, hit whenever
  `download::try_install_prebuilt` returns `Outcome::Fallback` — which fires
  on ANY prebuilt-download failure, network blip, 404, rate-limit, or SHA
  mismatch, not just an unsupported platform, so it is reachable even on a
  Tier-1 machine) called `trusty_common::update::perform_upgrade`, which also
  inherits its subprocess's stdout/stderr. Switched that call to the new
  `perform_upgrade_captured` (trusty-common 0.26.x) so it can never corrupt
  an active `LiveChecklist` either.
- **Post-install verify raced daemon startup instead of waiting for it**
  (#3833, demo-critical): `run_verify_tail`'s `launchctl kickstart -k` retry
  re-probed health exactly once, immediately — trusty-search notably
  downloads embedding models on first boot and can take tens of seconds, so a
  perfectly healthy machine reported `verify: NOT VERIFIED` / exit code 2
  moments before every daemon actually came up. `verify_one` now polls (new
  `poll_until_not_down`) on a bounded ~55-60s per-member schedule (up to 12
  attempts, 5s apart) instead of a single instant re-probe, printing a
  "waiting for services to start..." indication while it does. A member
  still `down` after the wait is now classified into one of three distinct
  end states (`DownState::NotLoaded` / `Crashed { exit_code }` /
  `StillStarting`, via `launchctl list <label>`) instead of a uniform,
  undiagnosable `down` — the per-row human summary now says e.g.
  `trusty-memory  down (not loaded)` or `trusty-search  down (crashed, exit
  78)`. The exit code / `verified` verdict already derived from the (now
  post-wait) `members` health, so both now reflect the FINAL state.
  - **Code-critic fix round**: the per-member poll folds SEQUENTIALLY over up
    to five launchd members, so five simultaneously-stuck daemons could stall
    `tctl install` for ~4-5 minutes — exactly the scenario an operator most
    needs a fast signal for. A new `AGGREGATE_POLL_BUDGET` (~120s total,
    checked fresh before every member) now bounds the WHOLE poll-wait phase,
    not just each member individually (`bounded_attempts` shrinks a late
    member's own attempt ceiling to fit what's left); once exhausted, the
    remaining members skip their poll (an instant probe only, flagged
    `budget_exhausted` in the JSON row) and `tctl install` prints one summary
    note — "poll budget exhausted — giving up on remaining members; re-run
    `tctl install` to check them again" — instead of repeating it per row.
- **`trusty-memory` was the only daemon member whose `service install` never
  bootstrapped the plist** (#3832, demo-critical — root cause lived in
  `trusty-memory`, see its CHANGELOG): `tctl install` shells out uniformly to
  `<binary> service install` for every launchd-managed member expecting it to
  write AND load the agent (matching `service_bootstrap`'s documented
  contract); trusty-memory alone only wrote the plist, so a fresh-machine
  install silently reported "trusty-memory: launchd service installed and
  bootstrapped" while `com.trusty.memory` never appeared in `launchctl list`.
  The verify tail's `down_state` classification above (`NotLoaded`) also
  makes this failure mode immediately diagnosable if it recurs for any
  member.
  - **Code-critic fix round (HIGH)**: `tctl install` downloads a component's
    LATEST PUBLISHED release binary, so the trusty-memory source fix above
    only protects a demo once a new trusty-memory release ships — an
    already-published (older) binary on a freshly-imaged machine would still
    hit the bug. `bootstrap_one` (`service_bootstrap.rs`) no longer trusts a
    clean `service install` exit code as proof of "loaded": it now
    independently checks `launchctl list <label>` (`ServiceEnv::is_loaded`,
    reusing the verify tail's own `launchctl list` primitive via the new
    `verify_tail::is_label_loaded`) and, if the label is NOT loaded despite a
    clean exit, force-bootstraps the already-written plist directly
    (`ServiceEnv::bootstrap_fallback`, mirroring `lifecycle.rs`'s minimal-
    `LaunchdConfig` pattern) — reported as the new
    `BootstrapAction::InstalledByFallback` ("bootstrapped by installer
    (component binary did not load its service)") rather than a silently
    inaccurate `Installed`. This wraps the NEW captured `run_service_install`
    (the `run_captured`-based call above, #3830) — the postcondition check
    runs after that captured call returns `Ok`, regardless of which
    implementation produced it. This makes the fix effective the moment
    installer 0.4.8 ships, independent of any component binary's release
    age, and guards the whole class of bug for every current and future
    service member — not just trusty-memory. Both the fallback-success and
    fallback-also-fails paths are unit-tested against `FakeServiceEnv`
    (`bootstrap_one_falls_back_when_not_loaded`,
    `bootstrap_one_reports_failure_when_fallback_also_fails`,
    `bootstrap_one_installed_directly_when_loaded`).

### Follow-up (tracked for a future release)

- `trusty-memory`'s `service_install()` bootstrap call (the binary-side half
  of #3832) has manual/integration verification only — there is no seam in
  `trusty_common::launchd::LaunchdConfig` equivalent to `service_bootstrap`'s
  `ServiceEnv` trait, so `install()` + `bootstrap()` sequencing across the
  four daemon crates' `service.rs` files cannot be unit-tested in isolation
  yet. (An earlier draft of this entry incorrectly claimed
  `tests/path_shadow_e2e.rs` covered it — that test file exercises the
  trusty-mpm supervisor bootstrap and PATH-shadow detection only, never
  `trusty-memory service install`.) A `LaunchdOps`-style trait seam mirroring
  `ServiceEnv`/`StubLaunchctl` would let this be unit-tested directly; left
  as a follow-up since it touches all four daemon crates. The NEW
  installer-side defensive fallback above IS fully unit-tested and is now the
  demo-critical safety net regardless of this follow-up's timing.

## [0.4.7] — 2026-07-23

### Added

- `tctl install` renders a live, in-place per-component progress checklist on
  an interactive terminal — one row per stable-set member, ticking `pending
  -> downloading -> verifying -> installed / skipped / failed` in place
  (`trusty-progress`'s new `LiveChecklist`) instead of scrolling `info:`
  lines. `--json` and any non-TTY/piped invocation (the `curl | sh` one-liner)
  are unaffected — the checklist is a zero-byte no-op there and the plain
  narrator-line sequence is unchanged.

### Changed

- Codesign / FDA (trusty-search) and App-Data-TCC (trusty-mpm) post-install
  guidance is now collected during the per-component install loop and printed
  ONCE, after every component has reached a terminal state, instead of
  interleaved mid-loop — keeps the live checklist (and the plain narration
  sequence) reading cleanly top-to-bottom instead of a multi-line advisory
  block interrupting it partway through.
- `tctl install`'s final verify-tail summary now tags an OPTIONAL daemon
  member as `(optional, skipped)` when its health is `down` OR
  `not_installed` (previously only `not_installed`) — an optional member
  that installed but whose service bootstrap failed no longer prints a bare,
  alarming `down` immediately above the green `VERIFIED` line; the
  verdict/exit code already tolerated this, this is annotation-only (#3797
  critic follow-up).

## [0.4.6] — 2026-07-23

### Fixed

- The `-y`/`--yes` non-interactive prereq install path (`tctl install -y`, and the `curl | sh -s -- -y` one-liner it powers) now actually runs the auto-install command for a missing prereq (`tmux`, `claude`) instead of degrading to a manual-only hint — the auto-install offer previously required a real TTY (`tty_fn()`) even under `--yes`, so a piped install (stdin never a TTY) silently skipped installing tmux/Claude Code despite the operator's explicit unattended consent. `--json` mode is unaffected: it remains fully non-interactive/no-auto-install.
- `tctl install tga` (and any transitive install that includes it) now resolves the correct release asset. `tga`'s release tag is `tga-v<version>`, but its release workflow names the tarball after the crate directory, `trusty-git-analytics-<version>-<target>.tar.gz` — the installer was building `tga-<version>-<target>.tar.gz` from the crate name alone, which 404s. Asset-filename construction now resolves through a small crate-name → asset-name alias table (tag resolution is unaffected).
- A from-scratch `tctl install` (the single-URL installer's default path) on a host with no Rust toolchain no longer fails the whole run with `installed 4/7 … exit 2 … NOT VERIFIED` when an OPTIONAL stable-set member (`trusty-analyze`, `trusty-console`, `tga`) has no prebuilt for the platform and cannot fall back to `cargo install`. Stable-set members are now classified REQUIRED (trusty-mpm + trusty-search + trusty-memory + trusty-review) vs OPTIONAL; an OPTIONAL member's install/health failure is reported as "skipped (no prebuilt for this platform)" and never flips the overall `all_ok`/`verified` verdict or the process exit code — only a REQUIRED member's failure does.

## [0.4.5] — 2026-07-21

### Fixed

- `tctl install`'s health gate and the trusty-mpm supervisor's candidate-version comparison now probe the CONCRETE just-installed binary path instead of resolving a bare binary name — previously, on a host with an earlier-PATH `~/.cargo/bin/tm` shadowing the fresh `~/.local/bin/tm` prebuilt install (the default `cargo install trusty-mpm` layout), both checks silently read the STALE binary via PATH/priority-directory lookup, so the documented `curl | sh` web-install upgrade path could report success while leaving the live `tm` and its supervisor on the old version (closes #3554). The health gate additionally cross-checks the probed version against the version the installer believes it just placed, hard-erroring on any mismatch instead of a silent pass.
- After a successful install, `tctl install` now detects whether the just-installed binary is actually the one a plain shell invocation resolves to. A genuine PATH shadow (an earlier, different copy of the same binary name) is reported loudly — naming both paths and both versions and what to do about it — and flips the member's outcome (and the process exit code) to a failure rather than a silent success (#3554).
- `tctl upgrade`'s two non-daemon branches now also run the PATH-shadow check above (#3554 review): they previously got only the concrete-path health-gate half of the fix, so an upgrade could report the correct new version while giving no warning that the shell still resolved a stale, PATH-shadowed copy.
- The PATH-shadow check's probe of the shadowing binary now goes through the same 10-second-timeout-guarded primitive the health gate uses, instead of an un-timed-out subprocess call (#3554 review) — an unresponsive shadowing binary (a stuck shell shim, a renamed/broken executable) degrades to an unknown shadowing version rather than hanging the unattended `curl | sh -s -- -y` install indefinitely.
- The macOS trusty-mpm supervisor bootstrap no longer unconditionally resolves the real UID's live `gui/<uid>` launchd domain — `plist_bootstrap::install_mpm_supervisor_for` now takes an injected `SupervisorTarget` (home dir, launchd domain, and a `LaunchctlPort`) instead of reading two independent env-var escape hatches (`TCTL_MPM_SUPERVISOR_HOME_OVERRIDE` / `TCTL_MPM_SUPERVISOR_SKIP_LAUNCHCTL`, both removed). Previously a test/E2E that overrode only the home directory still bootstrapped into the real, live supervisor domain, because the domain and the `launchctl` calls were never actually gated by the home override — the installer could not be test-isolated on a host running the live stack (closes #3551). `StubLaunchctl` provides a first-class, always-available way to exercise the full write path (plist write, downgrade guard, bootout + bootstrap) without ever spawning a real `launchctl` subprocess.

## [0.4.4] — 2026-07-20

### Added

- `trusty-mpm-gui` joined the `trusty-mpm` signable set in `SIGNABLE_BINARIES` (`com.trusty.trusty-mpm.gui`), so `tctl sign trusty-mpm` and the automatic `tctl install` post-install hook also sign the GUI binary (if present under the install dir) with the stable Developer ID identity — fallback coverage for a bare `cargo install --path crates/trusty-mpm-gui`, alongside the GUI's own `bundle.macOS.signingIdentity` fix (closes #2951).
- `tctl sign --verbose` (via the existing global `-v`/`--verbose` flag): prints the raw `security find-identity` probe output and which identity was selected, to help diagnose "no identity found" failures (closes #2939).
- `tctl install --dry-run` previews the blast radius (members, binaries, planned launchd service actions) and exits 0 without installing anything — identical behaviour in TTY, non-TTY, and `--json` contexts (closes #2112).
- `tctl install` now ends VERIFIED: after binaries + service bootstrap land, it automatically runs the `ensure` pass (`.mcp.json` patch + trusty-search index / trusty-memory palace registration) and a per-daemon health sweep, retrying a `down` launchd daemon once via `launchctl kickstart -k` before accepting the verdict (the #2498 "bootstrap succeeded but RunAtLoad never fired" flake). Prints a final green/red `VERIFIED`/`NOT VERIFIED` summary and folds the result into the same `--json` report / exit code CI already gates on. Skip with the new `--no-verify` flag (closes #2560).
- `tctl install --force` overrides the new trusty-mpm supervisor downgrade guard (see Fixed, below) to explicitly allow replacing a registered supervisor with an older-or-equal version.

### Changed

- **BREAKING for automation:** `tctl install` now REFUSES to run (exit 3) when stdin is not a TTY and neither `--yes` nor `--dry-run` was passed, instead of silently proceeding with a real install. Previously the blast-radius confirmation prompt was skipped entirely in non-TTY contexts (piped/scripted/agent-driven invocations), so an exploratory `tctl install` could reactivate real launchd services with no confirmation (#2112). Scripts and CI that relied on the old silent-proceed path must add `--yes`.

### Fixed

- `tctl install`'s trusty-mpm launchd supervisor bootstrap now honours `--no-service` / `TCTL_NO_SERVICE_BOOTSTRAP` like every other daemon member — previously it ran unconditionally regardless of the flag, which could bootout + replace a live production supervisor even when the operator explicitly opted out of touching launchd. It also now REFUSES to replace an already-registered supervisor with an older-or-equal version (comparing the registered vs. candidate `tm --version`, via `--force` to override), preventing a stale release from silently downgrading a newer live daemon (closes #3527).

### Fixed

- `has_developer_id_cert()` no longer passes a garbage identity string (SHA1 fingerprint + quotes still attached) to `codesign --sign` — it now extracts the quoted common name between the first and last `"` on the matching `security find-identity` line, matching `scripts/install-trusty-mpm-signed.sh`'s working `detect_identity()`. Previously `codesign` failed with "no identity found" even though the certificate was valid and listed by `security find-identity` (closes #2937, closes #2939).

### Fixed

- `tctl start`'s launchd skip message now says "already loaded" instead of "already running" — `is_loaded()` only confirms launchd registration via `launchctl print`, not process health, so a crash-looping agent (`KeepAlive::Always` restarting on every throttle interval) previously reported a misleading "already running" (closes #2573).

## [0.4.2] — 2026-07-17

Ships the real Developer-ID signed-install path — the working `tctl sign`
implementation that replaces the previous stub — into the wild (root cause
of #2834's stale-binary report).

### Added

- Unified Developer-ID signed install for search + tm, with plist bootstrap ([#2657](https://github.com/bobmatnyc/trusty-tools/pull/2657)) ([`7f249b3`](https://github.com/bobmatnyc/trusty-tools/commit/7f249b31350647d95cc3c28ae433ab78424a0e1c))
- Turnkey launchd bootstrap for the shared daemon set (closes #2557, #2556) ([#2566](https://github.com/bobmatnyc/trusty-tools/pull/2566)) ([`f47b428`](https://github.com/bobmatnyc/trusty-tools/commit/f47b4286aeb42d0a5871939edf0019a70cdfab78))

### Fixed

- Sign the `tm` binary too so App-Data TCC grant survives reinstalls (closes #2721) ([#2736](https://github.com/bobmatnyc/trusty-tools/pull/2736)) ([`95426c1`](https://github.com/bobmatnyc/trusty-tools/commit/95426c13bdcbf7dc61ba0f66703de0039fb2637b))
- arm64 Tier-1 + glibc-aware ORT asset selection for clean-env install ([#2282](https://github.com/bobmatnyc/trusty-tools/pull/2282)) ([`f9befad`](https://github.com/bobmatnyc/trusty-tools/commit/f9befad04fdf5cf9a93fefefa1e754aaa097c472))

### Changed

- Convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))
- Mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))

### Documentation

- Add missing package metadata to 7 crates ([#2293](https://github.com/bobmatnyc/trusty-tools/pull/2293)) ([`ee58b6a`](https://github.com/bobmatnyc/trusty-tools/commit/ee58b6a4ae01e1338e4761aaa5c27053c49f192b))

## [0.4.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
- Version reconcile: jumped 0.3.0 → 0.4.1, intentionally skipping past an
  already-published 0.4.0 that came from an orphaned/bad release commit.
