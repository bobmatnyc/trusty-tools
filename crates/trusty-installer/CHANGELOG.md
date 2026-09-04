# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.13.5] — 2026-09-04

### Fixed

- Release-tag resolution accepts either tag spelling for a crate whose tag prefix differs from its package name, so a `tga` pin resolves the `trusty-git-analytics-v<version>` tag the publish gate pushes instead of reporting the release as unpublished (#6771).
- When one version exists under both spellings and they name different commits or asset digests, resolution fails with a TAG-SPLIT error naming both tags rather than picking one.
- The "not a published stable release" message lists versions found under either spelling.

## [0.13.4] — 2026-09-02

### Breaking

- `commands::port_guard::parse_lsof_pids` is gone. The LISTEN-holder rewrite
  (#6264) replaced it with a parser that reads `lsof -FpT` per-file TCP state,
  so the old every-PID entry point has no caller and no meaning. Callers outside
  this crate must read the LISTEN holder through the guard's own entry point.
  This is what moves the version to 0.10.0 rather than 0.9.3.

### Added

- `trusty-installer` is a member of its own stable set, so
  `tctl install trusty-installer` works instead of answering
  `unknown member(s): trusty-installer`. It is a non-daemon and sorts last, so
  a bulk install replaces the running binary only after the rest of the stack
  has landed; both write paths rename atomically over the destination, so the
  running process keeps its open inode (#5805).
- Member names now resolve against every binary a crate installs, read from
  the shared `trusty_common::bin_resolve` table. `tctl install tctl`,
  `tctl install tm`, and `tctl install trusty-embedderd` resolve to
  trusty-installer, trusty-mpm, and trusty-search respectively (#5805).
- `tctl install --dry-run` names the destination directory it would write to —
  the canonical `$CARGO_HOME/bin` (#5777) — and lists every binary each member
  places, not just the one probed for health (#5805).
- `download::pinned::preflight_pinned_set` answers whether a pinned set could be
  installed on this host without installing it — a Tier-1 target check and a
  release-list lookup per tool, and no download, hash, or execution. It shares
  `resolve_pin` with `install_pinned_set`'s staging path, so a preflight cannot
  come to a different answer than the install it precedes, and a consumer that
  needed the question answered no longer has to install or grow a second
  resolver ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- **`ProbeOutcome::RpcError { code, message }`** — a UDS member that answered with a JSON-RPC error frame. A socket carries no HTTP status code, so `HttpError` has nothing to hold, and folding this into `BadEnvelope` would lose the coded reason: a client drifted onto a wrong method name (`-32601`) reads nothing like a handler that failed (`-32603`). Like `HttpError` it renders `down` and is NOT confirmed-down — the daemon accepted the connection and chose to refuse, which is evidence it is alive. `ProbeOutcome` is a public enum, so an exhaustive `match` on it downstream needs a new arm ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- **`uds_socket_for(binary)`** — resolves the Unix socket a member serves, or `None` for a member still on HTTP ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))

### Fixed

- `tctl stack doctor` and `tctl stack health` no longer report a member whose
  health is `unknown` as passing. Both gained a third verdict, `undetermined`,
  with its own exit code 4 — exit 2 still means a member is genuinely broken and
  exit 0 now means every member was determined healthy. A harness gating on
  these exit codes could previously go green over a stack that was not up
  (#4847).
- A `service install` that fails because the installed binary has no `service`
  subcommand is reported as release skew instead of forwarding clap's
  `unrecognized subcommand 'service'`. `tctl start trusty-console` against the
  published 0.4.0 now names the installed version, says the member has no
  supervised unit as a result, and gives the remedy, with the raw error appended
  (#4917).
- `ensure`'s basename-derived palace name is clamped to `PALACE_ID_MAX_LEN`, so a project directory with a long name no longer posts a name the daemon's format gate rejects ([#2443](https://github.com/bobmatnyc/trusty-tools/issues/2443))
- `tctl ensure`, `tctl port`, the readiness wait and project setup no longer
  fail against a healthy daemon when `HTTP_PROXY` is exported. `ensure`'s
  shared `build_client` now routes through
  `trusty_common::http_client::loopback_client_builder`, which applies
  `.no_proxy()`. `probe_http`'s own client routes through the same entry point,
  so the property this crate proved for `/health` in #4246 cannot drift from
  the rest of the workspace's loopback callers (#4392).
- `probe_member_health` now probes trusty-mpm's `/health` over HTTP instead of
  reporting it `unknown`. Probeability is a property of the daemon's HTTP
  transport, not of its lifecycle-management strategy; the two axes diverged when
  #4246 moved the probe from `<binary> health --json` to HTTP, and the
  `ManageStrategy::OwnVerb` arm was still keyed off the lifecycle enum. mpm now
  reports `healthy` when serving and `down` when not, in `tctl status`,
  `tctl stack health`, `tctl stack doctor` and the `tctl install` verify tail —
  and `tctl up` no longer issues a redundant `start` against a daemon already
  known to be answering.
  - **Behaviour change.** trusty-mpm is `required: true`, so a stopped mpm now
    yields `tctl status` / `tctl stack health` / `tctl stack doctor` →
    `degraded`, exit 2, and `tctl install` → NOT VERIFIED. This is intended: mpm
    becomes consistent with its declared `required` flag rather than exempt from
    it, exactly as a stopped trusty-search already behaves. CI that gates on
    `tctl status` exit codes may start failing where it previously passed; start
    mpm (`tctl start trusty-mpm`) or drop it from the gated set.
  - `ManageStrategy` is unchanged and still governs `tctl start|stop|restart`
    dispatch and `needs_kickstart`, so a confirmed-down mpm still cannot be
    handed to `launchctl kickstart -k` against the nonexistent `com.trusty.mpm`
    label.
- A prebuilt download that fails SHA-256 verification is no longer reported as
  "prebuilt unavailable" and silently rebuilt from source. `download::Outcome`
  now carries a distinct `ChecksumMismatch` variant, and `tctl install`,
  `tctl upgrade`, and `tctl self-update` all abort with an error naming both
  digests and the artifact URL instead of falling back to `cargo install`
  (#5518).
- `tctl install` no longer files an optional member's failed checksum under
  "skipped (optional, no prebuilt for this platform)" — in the checklist row or
  in the summary footer. An integrity failure now fails the run and its exit
  code regardless of whether the member is required.
- Stopped 14 doc comments in non-gated code from linking to macOS-gated items
  (`has_developer_id_cert`, `sign_binary`, `verify_signature`, `sign_set_strict`,
  `current_identifier`, `post_install_signed_set`, `apply_not_loaded_fallback`,
  `attempt_verify_fallback`), which rustdoc cannot resolve on Linux. docs.rs
  builds on Linux once per release and never rebuilds, so these would have been
  baked into the published documentation permanently.
- Prebuilt tarball installs place only the crate's expected binaries (per the
  shared `installed_binaries` table). Release tarballs ship mode-0755
  `LICENSE`/`README.md`, which previously landed in the bin dir as if they
  were binaries (#5777).
- The prebuilt tarball allowlist no longer drops unexpected executables
  silently: an extracted file that carries the execute bit, is not an obvious
  documentation file (LICENSE/README/CHANGELOG), and is missing from the
  shared `installed_binaries` table is now named in a warning before being
  skipped. Table drift used to produce a half-install with no trace at all
  (#5777, trusty-review round on PR #5778).
- `tctl install` no longer reports success when an all-OPTIONAL selection
  genuinely failed. The `all_ok` filter kept only REQUIRED members, and
  `.all()` over an empty iterator is vacuously true, so `tctl install tga` (or
  `trusty-analyze`, or `trusty-console`) printed `all_ok: true` and exited 0
  with nothing installed. A selection containing no REQUIRED member now gates
  on every member it selected (#5806).
- The post-install verify tail had the same vacuous truth one file over:
  `verified` collapsed to the `ensure` result for any selection whose daemons
  are all OPTIONAL, so `tctl install trusty-analyze trusty-console` printed
  VERIFIED with both daemons down. Both reports now read one shared gating rule
  (#5806).
- The human install summary contradicted the exit code. An all-optional
  selection that failed printed `installed 0/0 required component(s)`, an
  info-level "skipped", and a green VERIFIED while the process exited 2. The
  footer now counts the same gating set the exit code uses and prints those
  failures as errors (#5806).
- `InstallReport::build` over an empty member list reported `all_ok: true`.
  Nothing reaches it today — `run` returns early on an empty selection — but
  installing nothing is not evidence of a successful install (#5806).
- `tctl self-update` no longer reports success for a binary nothing has run. It
  printed `trusty-installer updated to X. Restart to apply.` and exited 0 as
  soon as the file was placed, so a broken or wrong-version download read as a
  completed update. It now runs `--version` on the exact binary it just wrote
  and cross-checks the reported version, matching the gate `tctl install` has
  always applied to every other member; a failed probe or a mismatch is an error
  and exit 1, never a cargo fallback (#5807).
- A pinned install no longer places a partial set and calls it complete. When a
  binary the set named was absent from the staged extraction directory,
  `copy_set_into_install_dir` skipped it silently and still returned `Ok`, so
  the caller received a set missing a tool with no warning anywhere. The copy
  phase now fails closed on that name, before the first commit rename, so its
  "nothing was installed" text stays true (#5810).
- The `tctl install` footer now reports a PATH-shadowed component. A member that
  installed and bootstrapped cleanly but is shadowed by a different, earlier
  copy of the same name gave `all_ok: false` and exit 2 under a footer reading
  `installed 1/1 required component(s)` with no error line. Shadowing was the
  last `all_ok` input the footer left out; the human summary and the exit code
  now agree on every input (#5812, completing #5806).
- The pinned-tool installer retries the release-list lookup when the crate is
  published and only the requested version is missing, and says so when the
  retries run out. `trusty-audit audit` failed hard half an hour after
  trusty-review 0.23.0 went live, naming `0.22.1` as the newest published
  version; a manual retry with no other change succeeded, and the message gave
  no hint that waiting was the answer. A crate with no published releases at all
  is still refused on the first answer — that is a typo, not lag.
- The #4470 foreign-port guard now names the process holding a daemon's port in
  LISTEN, not the first process `lsof` printed. A client connected TO the port
  is not a holder of it, and `lsof` lists in PID order, so any client older than
  the daemon sorted ahead of it — `tctl install` then refused to bootstrap
  trusty-search with "port 7878 is held by pid ..., which launchd does not
  supervise" while the launchd-supervised daemon was the actual listener. The
  probe now asks `lsof` for the per-file TCP state (`-FpT`) and confirms LISTEN
  itself instead of trusting the `-sTCP:LISTEN` selector to have filtered.
  - Fail-closed is unchanged: output naming no LISTEN holder is `Unknown` and
    still refuses, and an `lsof` build that reports no TCP state at all falls
    back to the previous every-PID reading rather than going blind.
  - The per-service port table is unchanged. Every daemon `tctl` guards
    (trusty-search, trusty-memory, trusty-analyze, trusty-review,
    trusty-console, the trusty-mpm supervisor) still binds a TCP port in its
    current source, so no entry was stale.
- **`tctl ensure` stopped provisioning memory palaces, and reported it as a skip.** The palace stage asked `resolve_base_url(MEMORY_APP)`, which reads an `http_addr` file ADR-0032 stopped writing — so it answered `None` on every machine and the stage printed "trusty-memory daemon not running; skipped" whether or not one was, giving a green report for a project that was never provisioned. It probes trusty-memory's socket and calls `memory.palace_get` / `palace_create` through the shared client now ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286))
- **`tctl ensure --wait` could never report ready.** Its probe required the same dead resolver to answer, so the memory arm was `false` on every iteration: the command ran its whole 120-second budget and exited 4 no matter what the stack was doing. The memory arm now asks whether anything is serving the socket
- A `memory.palace_get` failure that is not a not-found no longer falls through to a create. Only "no such palace" means the palace has to be made; creating on top of any other refusal turned one reportable failure into a second
- `tctl install` and `tctl upgrade` evict the launchd unit trusty-analyze left
  loaded, through the same `RETIRED_SERVICES` mechanism that clears
  `com.trusty.review` (#6290) — one eviction path, now two rows. Without it an
  upgraded macOS host kept `com.trusty.analyze` loaded under `KeepAlive: Always`,
  which restarted the on-demand trusty-analyze server every time its idle window
  reclaimed it. A unit that will not go down fails that member's install or
  upgrade rather than being reported as a skip (#6350).
- `tctl start`/`restart`/`upgrade` no longer route trusty-analyze through
  launchd. Its retired row makes `manage_strategy_for` return
  `ManageStrategy::None`, so `start` cannot bootstrap the retired unit on an
  upgraded host or shell out to a `trusty-analyze service install` that no
  longer exists on a fresh one. The port guard could not have caught either:
  analyze has bound no port since #6287, so the guard permits vacuously (#6350).
- `tctl status` reported a healthy trusty-memory daemon as `down`, exited 2 with verdict `degraded`, and failed `tctl install`'s verify tail (#6555). The UDS health probe sent a JSON-RPC frame with no `params` key, which decodes to `Value::Null`; `memory.health` is bound to a real `HealthQuery` struct and refused it with `-32602`. The probe now sends `"params": {}`, which is what the correct dialers already send. The same frame serves `analyze.health` and `search.health`, whose `NoParams` binding had been tolerating the omission
- `tctl restart <service>` waits for launchd to finish the bootout before it
  bootstraps. The Restart arm called `bootout()` then `bootstrap()` back to back;
  because `launchctl bootout` returns when the unload is accepted rather than
  when the job is gone, the bootstrap reached launchd with the label still
  registered and was refused — `trusty-memory: booted out successfully;
  bootstrap failed: … Bootstrap failed: 5: Input/output error` — while a plain
  retry seconds later succeeded (#6618). It now routes through
  `trusty_common::launchd::LaunchdConfig::restart_gracefully`, which quiesces a
  unit whose loaded `ExitTimeOut` is shorter than the daemon's flush (the #6590
  guard `service install` already ran), waits the label out of launchd, and
  retries the bootstrap once after a second wait. A restart that still fails
  names the label, both waits, and launchd's own reason.
- The success line reports the wait it paid and how many bootstrap attempts it
  took, so a race that recurs is visible instead of silently absorbed.
- `stack doctor` (and every other `tctl` health rollup) no longer reports
  `trusty-memory … health:down` against a healthy daemon. The UDS probe built
  its `memory.health` request with no `params` field, which decodes
  server-side as `Value::Null`; `HealthQuery` is a plain derived `Deserialize`
  that refuses `null` however many of its fields carry `#[serde(default)]`, so
  every probe answered with a coded `invalid_params` error that rendered
  `down`. The request now carries an explicit `"params": {}`, matching the fix
  `trusty-console`'s own probe already carries for the identical mismatch
  (#6356) ([#6630](https://github.com/bobmatnyc/trusty-tools/issues/6630)).

### Changed

- `MEMORY_SET` no longer lists `trusty-bm25-daemon`. #5329 removed that binary from trusty-memory's install surface, so `tctl sign trusty-memory` signs `trusty-memory` and the deprecated `trusty-memory-mcp-bridge` shim only.
- Every install/upgrade write path now targets the canonical cargo bin dir
  (`$CARGO_HOME/bin`, falling back to `~/.cargo/bin`) instead of
  `~/.local/bin` (#5777, #4964 Phase 3): `download::default_install_dir()`
  delegates to the shared `canonical_bin_dir()`, `install.sh`'s default is
  `${CARGO_HOME:-$HOME/.cargo}/bin`, `tctl self-update` places into the
  canonical dir rather than `current_exe()`'s parent, and the deliberate
  `~/.local/bin` + `~/.cargo/bin` double-write in trusty-agents'
  `install-wrapper.sh` is deleted. Binaries no longer land in two directories
  with PATH order deciding which copy runs — the stale-daemon mechanism
  behind #2386. The unused `DEFAULT_INSTALL_DIR` const is removed.
- Membership reaches four more subcommands, not just `install`. A bare
  `tctl upgrade` now replaces the running installer along with the rest of the
  stack, and `--exclude-self` is the opt-out — before membership that flag
  matched nothing and did nothing. `tctl updates` reports the installer's own
  staleness, and `tctl status` lists it as a non-daemon row (`health: n/a`).
  The daemon-shaped subcommands — `start`, `stop`, `restart`, `stack health`,
  `stack doctor` — are unaffected: they filter on `daemon`, and the installer
  is not one (#5805).
- `tctl config` no longer forwards to the installer. Membership made the
  fan-out spawn `trusty-installer config --json`, which is `tctl config`, which
  enumerated the set and spawned it again — unbounded recursion on the
  documented bare "all members" form. The installer is listed as `skipped`
  instead, and naming it alone (`tctl config trusty-installer`) is a usage
  error rather than an empty success (#5805).
- The PATH-shadow check covers every binary a member places, not just the one
  probed for health. A stale `tctl` earlier on `$PATH` used to win every shell
  invocation while the install reported clear (#5805).
- A forwarded contract verb (`<binary> config --json`, `<binary> version
  --json`) gives up after 10 seconds. It ran with no deadline, so a member
  binary that never exits froze `tctl config` indefinitely with nothing on
  stdout (#5805).
- A tarball missing a binary the shared `installed_binaries` table lists is
  reported at warning level. The preview names every binary a member places, so
  a tarball shipping fewer used to over-promise and under-deliver silently
  (#5805).
- `tctl install`'s human summary now reads the gating partition from
  `stable_set::gating_split` rather than re-deriving `required || !any_required`
  inline, so the footer and the `all_ok` verdict share one expression of the
  rule and cannot drift apart again (#5806).
- **The health probe dials trusty-review over its Unix socket.** `fixed_port_for("trusty-review")` no longer returns 7891; `uds_socket_for` resolves the socket instead, and a member with one is probed only over it — there is no second leg to reconcile and no discovery file to go stale. Leaving the probe on 7891 would have read `Refused` for a healthy daemon, and `Refused` is one of the two variants that authorise `launchctl kickstart -k`, so every `tctl install` would have hard-restarted a working review daemon — #4246 again, which is why this shipped with the daemon change (ADR-0032, [#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- `classify_rpc_response` applies the same body-first envelope rule as `classify_response`, so `effective_status`'s warming/degraded downgrade and the squatter check are unchanged across the two transports ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- `resolve_guard_ports` returns no ports for a member that binds a Unix socket. Without that, the `http_addr` file left behind on any machine that ran trusty-review before the swap would still resolve `7891`, and the #4470 bootstrap guard would refuse an install because an unrelated process held a port trusty-review never binds ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- `tctl`'s health probe now asks trusty-search for `search.health` on its Unix
  socket, the transport #6285 is moving the daemon onto, instead of reading
  `GET /health` alone. Both legs are read while trusty-search serves both: no
  published trusty-search binds a socket yet, so a socket-only probe would
  report every installed daemon as `Refused` — the verdict that arms `launchctl
  kickstart -k` — and hard-restart a healthy search mid-index-flush on every
  `tctl install`. `probe_http::dual_transport` marks that window, and its arm is
  deleted when the axum surface is (#6285)
- Bumped version to 0.13.2 to replace the stranded `trusty-installer-v0.13.1`
  tag, which was cut at a commit that cannot pass the rustdoc intra-doc-link
  publish gate. No code change (#6285).
- `tctl` probes trusty-memory over its Unix socket (`memory.health`) rather than dialling `7070` (#6286, ADR-0032). `fixed_port_for("trusty-memory")` is gone: leaving it would make every `tctl install` dial a port the daemon no longer binds, read the refusal as confirmed-down, and `launchctl kickstart -k` a healthy daemon — #4246 verbatim
- The port guard resolves NO port for trusty-memory, including when a pre-migration `http_addr` is still on disk. That file is deleted by the daemon at every start, but only once it has started, and the guard runs before that
- `tctl`'s health probe reaches trusty-analyze over its Unix socket rather than
  port 7879 (#6287, ADR-0032). A probe left on the retired port reads `Refused`,
  which `is_confirmed_down` accepts and `verify_tail` turns into
  `launchctl kickstart -k` against a daemon that was working — the #4246 class.
- `uds_health_method` generalises the hardcoded `review.health` constant to one
  method per binary, so a second UDS member cannot inherit the first one's name.
- Dropped `SUPERVISOR_METRICS_PORT`, the `LaunchctlPort::port_guard` seam, and the `TRUSTY_MPM_SUPERVISOR_ADDR` key from the supervisor plist template. The supervisor binds no port since #6288, so there is none for a foreign process to hold (Refs #6288).
- `tctl` no longer treats trusty-review as a daemon (#6290): it never shells out
  to `trusty-review service install`, and boots out the retired
  `com.trusty.review` unit (and its `com.trusty.trusty-review` alias) during
  `tctl install`'s service-bootstrap pass instead.
- The member is probed by presence — binary on PATH plus `--version` — rather
  than by dialling a socket nothing binds. A presence probe can never report
  confirmed-down, so `launchctl kickstart -k` can no longer fire at a label that
  does not exist.
- The eviction now actually runs. `plans_service_bootstrap` gated on
  `manage == Launchd`, and a retired member is `ManageStrategy::None`, so the
  install loop skipped it and `com.trusty.review` was never booted out on any
  host. It is now visited when the member has a retired service.
- A retired unit that will not go down fails the install instead of being
  reported as a skip, so the exit code shows it.
- `tctl upgrade` evicts a retired member's unit too, through the same
  mechanism. `restart_plan` read the member as "not a daemon" and skipped it, so
  an upgrade left `com.trusty.review` loaded and respawning; only `tctl install`
  cleared it. A unit that will not go down now fails that member's upgrade.
- `tctl` no longer expects trusty-analyze to be a resident daemon.
  `member_has_service_install` excludes it (its `service install` subcommand is
  gone), and `tctl up`'s analyze stage now starts it on demand and pings
  `analyze.health` once instead of gating on a live dial — so a correct
  installation with nothing listening reports Ok rather than Degraded (#6350).
- The health probe follows. A retired member is `ManageStrategy::None`, which
  would have made analyze `Unprobeable`; it keeps its transport probe through
  the same named-predicate arm #6290 added for a presence-only member, because
  starting it and asking `analyze.health` is the only check that separates
  "working" from "broken" for a service whose healthy resting state is not
  listening (#6350).
- trusty-installer moves to 0.13.0. `cargo-semver-checks` reports five major
  lints against the published 0.12.0 baseline, all of them already on `main`:
  `pub_module_level_const_missing` (`SUPERVISOR_METRICS_PORT`),
  `inherent_method_missing` and `struct_pub_field_missing`
  (`StubLaunchctl::port_guard_calls` / `refuse_port`) and `trait_method_missing`
  (`LaunchctlPort::port_guard`) from #6349, plus `trait_method_added`
  (`ServiceEnv::evict_retired`) from #6290. For a `0.y.z` crate the breaking
  bump is the MINOR position, so 0.12.1 was never a legal position for that
  work. The root workspace requirement moves from `0.12` to `0.13` (#6350).

### Removed

- `walk_range_for` and the walk-range branch of `resolve_guard_ports`. trusty-memory was the only member that walked (`7070..=7079`) and it serves a socket now, so nothing produced a range. `decide_over_range` still iterates whatever `resolve_guard_ports` returns, which is how a future walker gets the #4470 relaxation back

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- `stable_set.rs` referred to the report module as `super::install_report`. The
  module is declared inside `install.rs`, so its real path is
  `commands::install::install_report` and the link resolved to nothing. Both
  references are plain code text now (#5973).

## [0.8.0] — 2026-08-12

### Added

- New public `download::pinned` entry point installs prebuilt binaries at
  caller-specified EXACT versions and fails closed. `install_pinned_set` takes a
  set of `PinnedTool` pins, verifies each artifact against the published
  SHA-256 (and, optionally, a caller-supplied digest), verifies the downloaded
  binary itself reports the pinned version, and installs all tools or none —
  nothing reaches the install directory until every tool has verified. Every
  failure is a typed `PinnedError` naming what was pinned, what arrived, and
  that nothing was installed; `cargo install` is unreachable from this path
  (#5491)
- Version bumped 0.6.0 → 0.7.0 in this PR. The change is purely additive — a
  new module, new types, and `release::resolve_pinned_tag` — so a MINOR bump
  under Cargo's 0.x rules. `scripts/check_semver.sh --crate trusty-installer`
  confirms no breaking change against the published baseline (#5491)

- `install_pinned_set` places the whole set with copy-then-commit: every binary
  of every tool is copied into the install directory under a hidden temporary
  name, and nothing is renamed to its final name until all copies succeed.
  Previously phase 2 placed tools one at a time, so a failure on tool 2 returned
  an error reading "nothing was installed" while tool 1's binary was already on
  disk (#5517)
- New `PinnedError::PlacementInterrupted` covers the one case where files do
  survive a failure — a commit rename failing after an earlier one succeeded. It
  lists every file left in the install directory and every crate they belong to,
  instead of reusing the `Io` variant's "nothing was installed" text. Every other
  variant now provably means no file was placed under its final name (#5517)
- A set whose tools install two binaries to the same path, and a destination
  already occupied by a directory, are both rejected before anything is copied
  (#5517)

- Check 5 EXECUTES the downloaded binary to read its `--version`, so a pinned
  install runs a freshly downloaded, not-independently-signed artifact in the
  installer's own process and user context during an unattended install. The
  digest gating it is self-published by the same release pipeline that would be
  compromised in the attack this guards against. The module doc records this as
  an accepted trade-off: a mis-tagged or mis-built asset passes every URL- and
  digest-level check, and executing the binary is the only thing that catches it
  (#5517)
- `download::try_install_prebuilt` is deliberately UNCHANGED. It still resolves
  `latest` and still returns `Outcome::Fallback` to `cargo install` on any
  failure, because `install`/`upgrade` depend on that behaviour. The pinned path
  is a separate entry point rather than a change in its semantics (#5491)
- `download::http_client()` is public. `download::pinned::install_pinned_set` takes a `&reqwest::Client`, and its out-of-crate caller had no way to obtain one without answering "how is this workspace's download client built?" a second time. Consumers now call this and need no `reqwest` dependency of their own ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))

### Fixed

- `plist_label_for` no longer keeps its own table of launchd labels. It kept a
  `com.trusty.<binary>` convention plus hand-added overrides, held in step with
  the daemon crates by grepping their `LAUNCHD_LABEL` constants — and it had
  drifted: it resolved trusty-search to `com.trusty.trusty-search` and stated
  the convention "is correct" for it, while the loaded unit is
  `com.trusty.search`. Every `tctl start`/`stop`/`restart` bootout and every
  `tctl stack doctor` plist-presence check therefore targeted a job that does
  not exist. It now delegates to `trusty_common::launchd_labels`, which the
  daemons read too (#4868)
- The supervisor plist template carried `com.trusty.mpm.supervisor` as its own
  literal beside `PLIST_LABEL`. Two copies of a label are two things that can
  disagree, and a plist whose `Label` key differs from the label the installer
  boots out is the #2827 defect landing on the unit that restarts everything
  else. The template now fills the label from the constant, and the test asserts
  on the rendered output rather than the raw template (#4868)
- `plist_label_for` consults the service registry before falling back to the
  naming convention, so a member whose only unit is a SUB-unit resolves
  correctly — `trusty-agents` returned `com.trusty.agents` while the loaded unit
  is `com.trusty.agents.slack`, which would have had `tctl` target a job that
  does not exist (#4868)
- `tctl upgrade` no longer installs every daemon member twice. On the prebuilt
  path the binary was already on disk when the daemon branch went on to call
  `upgrade_and_restart`, whose first step is `cargo install <crate> --locked` —
  a second copy, in a second directory, from one command. The comment saying
  that step was "a no-op if the binary is already current" was wrong: cargo
  skips only when its own `.crates2.json` records that exact version, and the
  prebuilt path writes no cargo metadata. Six of the seven stable-set members
  are daemons, so this fired on nearly every upgrade — and on a machine with no
  Rust toolchain it errored out after the new binary had already landed
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `tctl upgrade` now actually restarts a daemon member. `upgrade_and_restart`
  restarts by calling `std::process::exit(1)` and letting launchd's `KeepAlive`
  respawn the process that just exited — correct for `trusty-search upgrade`,
  `trusty-memory upgrade`, and the two MCP `upgrade` tools, which all run inside
  the supervised daemon, and a guaranteed no-op for `tctl`, a terminal process
  launchd has never heard of. The supervision check evaluated `tctl`, returned
  false every time, and the manual-restart hint it produced was reported as
  success, so the daemon kept serving the old process indefinitely. Both daemon
  branches now bounce the member through the same launchd path `tctl restart`
  uses (port-guard, then `bootout`, then `bootstrap` — never `kickstart -k`).
  Note the bounce re-execs whatever path the plist's `ProgramArguments[0]`
  names, and nothing yet rewrites that — so on a host whose plist was baked
  from the other bin directory the daemon comes back on the same old binary.
  Phase 4 of the epic regenerates the plist; this change stops the daemon being
  left un-bounced, it does not yet guarantee which binary it comes back on
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `tctl install` passes the concrete path of the binary it just wrote to a
  member's `service install`, instead of a bare name resolved through `$PATH`.
  The spawned process bakes its own `current_exe()` into the launchd plist's
  `ProgramArguments[0]`, so a stale copy winning the `PATH` lookup persisted
  that stale path into launchd, which then respawned it at every boot with
  nothing to rewrite the plist
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- The component table's size column reads the binary this run just placed. It
  joined the binary name onto the cargo bin dir while the prebuilt path writes
  elsewhere, so it reported a stale copy's bytes, or zero
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `install_all`'s install-directory fallback reads `CARGO_HOME`. It hardcoded
  `~/.cargo/bin` while the sibling fallback in `install_one` — same job, same
  file — did read it. Both, plus `tctl sign`'s `--dir` default,
  `tctl self-update`'s cargo-destination check, and `tctl upgrade`'s health-gate
  path, now share `trusty_common::bin_resolve::canonical_bin_dir`
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- Corrected the claim that `~/.local/bin` is preferred "to avoid cdhash issues
  on macOS". What keeps the cdhash cache consistent is the atomic rename in the
  download layer, which holds in any directory
  ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
- `trusty-memory` and `trusty-analyze` are now signable targets. `tctl sign trusty-memory` signs all three binaries `cargo install --path crates/trusty-memory` produces (`trusty-memory` → `com.trusty.trusty-memory`, `trusty-bm25-daemon` → `com.trusty.trusty-bm25-daemon`, `trusty-memory-mcp-bridge` → `com.trusty.trusty-memory-mcp-bridge`); `tctl sign trusty-analyze` signs its single binary (`com.trusty.trusty-analyze`). Both were absent from `SIGNABLE_BINARIES` for no recorded reason: `trusty-memory setup`/`migrate` and `trusty-analyze setup --global` walk `$HOME` to depth 8 for `.claude/settings*.json`, and that walk skips `Library`/`Applications` but not `~/Desktop`, `~/Documents`, or `~/Downloads` — the same cdhash-keyed TCC exposure that put `trusty-search` ([#873](https://github.com/bobmatnyc/trusty-tools/issues/873)), `tm` ([#2721](https://github.com/bobmatnyc/trusty-tools/issues/2721)), and `tagent` ([#4277](https://github.com/bobmatnyc/trusty-tools/issues/4277)) in the table. `trusty-analyze`'s walk sits behind `--global` rather than its default path, so the exposure is narrower in how often it is reached and identical when it is. New `scripts/install-trusty-memory-signed.sh` / `make install-memory-signed` cover trusty-memory's local-source install; `tctl sign trusty-analyze` is trusty-analyze's whole surface. `trusty-review` was evaluated under the same test and excluded — it makes no `$HOME` walk and reads no other application's files.
  - Hardened Runtime follows `trusty-search`'s conservative split (explicit `tctl sign` only, not the automatic hook) for both: `trusty-memory` links `ort`/`fastembed` through `trusty-common`'s `memory-core` and `trusty-analyze` links it directly, so both carry the same unverified ONNX-dylib-under-library-validation exposure.
  - Signing establishes a stable designated requirement — the precondition for a durable TCC grant, not proof one was obtained. The script and `docs/reference/release-workflow.md` now say so, and record that macOS attributes Files-and-Folders access to the responsible process (usually the terminal for a shell-invoked CLI, which is exactly the `setup`/`migrate` path), leaving the CLI-path outcome unresolved.
  - The `tctl sign` targets named in `signing_persistence_tip` had gone stale after [#4277](https://github.com/bobmatnyc/trusty-tools/issues/4277) (`trusty-agents` was missing). The tip's advertised list is now parsed and compared exactly against `SIGNABLE_BINARIES`, and each `SignTargetArg` variant's clap `#[value(name = …)]` is pinned against its `as_set_name()`, so a set can no longer be added to the table and left unreachable from the CLI by a typo on either side.
- `trusty-analyze` is no longer routed to the `x86_64-linux-al2023` /
  `aarch64-linux-al2023` assets on a below-glibc-floor host. It stopped
  bundling ONNX Runtime when its unused neural embedder was removed, so the
  release workflow no longer publishes an AL2023 variant for it and the old
  routing would have 404'd on an asset that is never built (#5067)
- **Breaking — version bumped 0.5.1 → 0.6.0 in this PR.** The public const
  `download::glibc::ORT_CRATES` narrows from `[&str; 2]` to `[&str; 1]`. Array
  length is part of the type, so any downstream binding it as `[&str; 2]`
  stops compiling, and under Cargo's 0.x rules a break takes the MINOR
  position. **`cargo semver-checks` does NOT catch this shape** — it has no
  lint for a `const`'s type changing and reported `196 checks: 196 pass, 58
  skip / no semver update required` against the 0.5.0 baseline. Read that
  green as absence of coverage, not as evidence of compatibility; the bump was
  made deliberately rather than left for the releaser to notice, because a
  fragment missed at cut time is exactly how #4088 turned into a yank
  (#5067, #4088)
- `download::pinned::install_pinned_set` installs executables only. It placed every regular file in the archive, and every release tarball in this workspace ships a `LICENSE`, so a multi-tool set collided on one destination filename and refused — the three-tool `tga` / `trusty-analyze` / `trusty-review` set could never install. A genuine two-binaries-one-name conflict is still an error ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))

### Changed

- **The stable-set daemon-member rule now lives behind one entry point, `stable_set::daemon_members`, and its result is pinned by name.** `stack doctor` and `stack health` each carried an independent copy of the same `stable_set().into_iter().filter(|m| m.daemon)` expression, and no test in any crate exercised either one.
  - This matters beyond `tctl`: `vmtest-harness` deliberately **derives** its daemon-liveness set from `stack doctor --json`'s member table rather than transcribing a list (`_verify_daemon_set`, `vmtest-harness/lib/verify.sh`). Narrowing the filter therefore shrank the harness's oracle silently — the dropped daemon stopped being probed, its name landed on the already-noisy `unreported` log line, and the run still reported PASS.
  - `tests::daemon_members_is_pinned` pins the six daemon crate names in stable-set order, so a narrowing is a deliberate, reviewed edit rather than accidental drift. It pins the NAMES, not the filter expression, which would restate the implementation and catch nothing.
  - `stack doctor <member>`'s named-member arm routes through the same entry point, so the daemon scoping cannot diverge between the two arms.
  - Adds `tests::unknown_member_exits_3`, the unknown-member error path the `doctor` module header has claimed coverage for since the file was written; it also asserts a real but non-daemon member (`tga`) is rejected with exit 3 rather than sweeping the whole stack.
- **MSRV raised to Rust 1.94** (was 1.91). `aws-config` >= 1.9.0 and
  `aws-sdk-bedrockruntime` >= 1.136.0, published 2026-07-08, declare
  `rust-version = "1.94.1"`; because those are unpinned caret ranges in the
  workspace manifest, `cargo install` **without `--locked`** re-resolves into
  them and then refuses to build on rustc below 1.94.1 — the reported
  `cargo install trusty-code` failure on rustc 1.91.1. Users on rustc
  1.91-1.93 must `rustup update` before installing any `trusty-*` crate. See
  [ADR-0029](../../docs/adr/0029-msrv-1-94-and-edition-policy.md)
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))

### Documentation

- The `kickstart -k` hazard notes on `probe_http` and `verify_tail::needs_kickstart`
  described the shared plist renderer as emitting no `ExitTimeOut` and launchd
  as SIGKILLing 20 s after SIGTERM. The renderer now declares the key (#4393),
  and the pre-fix default measured 5 s, not 20 s. No behaviour change — the
  confirmed-down gate is unaffected (#4393)

## [0.5.0] — 2026-08-04

### Added

- **`tctl sign trusty-agents`** — extended the Developer-ID codesign single source of truth (`macos_signing::SIGNABLE_BINARIES`, [#2558](https://github.com/bobmatnyc/trusty-tools/issues/2558)) with a new `AGENTS_SET` covering the `tagent` CLI binary (`com.trusty.tagent`), so `cargo install --path crates/trusty-agents`'s ad-hoc-signed binary can get a stable Developer ID identity the same way `trusty-search`/`trusty-mpm` already do, fixing macOS re-prompting a TCC category on every rebuild ([#4277](https://github.com/bobmatnyc/trusty-tools/issues/4277)). `tctl sign`'s `<TARGET>` now accepts `trusty-agents` alongside `trusty-search`/`trusty-mpm`, and `AGENTS_SET` joins `MPM_SET` on the always-Hardened-Runtime side of `use_hardened_runtime` (`tagent` loads no ONNX dylib, so the library-validation concern that keeps `trusty-search` off it does not apply).
- New public `probe_http` module: an HTTP `/health` probe local to trusty-installer, replacing the old CLI-verb probe ([#4246](https://github.com/bobmatnyc/trusty-tools/pull/4246), [#4381](https://github.com/bobmatnyc/trusty-tools/pull/4381)).

### Fixed

- On an arm64 Linux host with glibc below 2.39, `trusty-search` / `trusty-analyze` installs now download the new `aarch64-linux-al2023` release asset instead of the `aarch64-unknown-linux-gnu` one, which is built on Ubuntu 24.04 and cannot load there (Amazon Linux 2023 on Graviton being the confirmed case). Mirrors the existing x86_64 routing (follow-up to [#2533](https://github.com/bobmatnyc/trusty-tools/issues/2533), predecessor [#4822](https://github.com/bobmatnyc/trusty-tools/pull/4822)).
- **`launchctl bootstrap` no longer reports success when a foreign process already holds the daemon's port** ([#4470](https://github.com/bobmatnyc/trusty-tools/issues/4470)). Root cause: `bootstrap` exits 0 regardless of whether the job it loads can actually bind, so an unsupervised orphan — typically an older binary — kept serving the port while the documented `bootout → install → bootstrap` restart convention completed cleanly. A fresh signed install plus behavioural verification could all pass while shipping nothing ([#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230) records the near-miss on the 1.2.3 deploy). [#4466](https://github.com/bobmatnyc/trusty-tools/pull/4466) detected the state after the fact via `tm doctor`'s `daemon_orphan` check but nothing intercepted the bootstrap itself.
  - New `port_guard` module refuses a bootstrap whenever the member's port is held by a PID that launchd does not report running for that label. The refusal names the offending PID, the port, and the `lsof` / `kill -TERM` recipe to clear it, and `tctl install` exits non-zero with `all_ok: false` rather than reporting a refused member as a success.
  - It **fails closed**: an unreadable port probe, or a `launchctl` query that cannot be answered, is a refusal — not a pass. Because a non-root `lsof` only reports the caller's own processes, an empty `lsof` result is cross-checked with a real bind, so another user's (or a root-owned) listener is reported as an unidentifiable holder rather than a free port. `TCTL_ALLOW_FOREIGN_PORT=1` is the single documented operator override; it requires an affirmative value (so `=0` cannot switch the bypass *on*), is named in every refusal message, and logs loudly when used.
  - All **four** bootstrap-issuing sites route through the one shared check: `tctl install`'s service bootstrap (both the fresh-install and the [#3841](https://github.com/bobmatnyc/trusty-tools/issues/3841) already-present-plist repair branch), the verify tail's second-layer repair, `tctl start` / `tctl restart`, and the trusty-mpm supervisor plist bootstrap. A new `bootstrap_sites` test enumerates the bootstrap sites actually present in the source and fails on any that is unaccounted for, so a fifth site cannot silently bypass the guard.
  - Every gate runs **before** any bootout or filesystem write, so a refusal never stops a running daemon and then declines to bring it back, and never leaves a half-written plist behind.
  - The port checked is the member's recorded `http_addr` when it has one. Otherwise a daemon that auto-walks (trusty-memory walks `7070..=7079`) is checked across its whole range and refused only when every candidate is unavailable, so an unrelated listener on the default port no longer blocks an install that would have succeeded.
  - `launchctl list` observations now distinguish "launchd says no such label" from "launchd could not be asked" — the conflation [#4466](https://github.com/bobmatnyc/trusty-tools/pull/4466) identified as a HIGH on the trusty-mpm side — and carry the running PID, off the same single parser the existing down-state diagnosis uses.
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
