# Changelog — trusty-installer

All notable changes to trusty-installer are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

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
