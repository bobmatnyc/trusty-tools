# Changelog — trusty-browser

All notable changes to trusty-browser are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

Unreleased changes live as fragments in `changelog.d/`.

---

## [0.9.0] — 2026-11-27

### Fixed

- `trusty-browser --version` and `trusty-browser-mcp --version` now print the crate version and exit 0; both were refused as an unexpected argument (#11802).
- `install-host`, `install-service` and `launchd-plist` never record a cargo build-directory binary: they register the `cargo install` binary instead, or refuse with the rejected path and the install command ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- The native-host launchers answer a missing binary with an error naming its path and the repair step, so the popup's Retry shows it instead of "Native host has exited" ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- `install-service` and `uninstall-service` take over a launcher or plist whose binary is gone (not found; an unreadable binary still counts as present), and the launcher form 0.8.2 wrote, instead of refusing them as unrelated ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- `uninstall-service` run from a build binary also matches the registration of the installed binary it would substitute, and its own ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))
- A binary in any cargo profile directory (such as `target/ci-smoke/`) counts as a build path; a `debug` or `release` segment deeper under a `target` directory no longer does ([#11927](https://github.com/bobmatnyc/trusty-tools/issues/11927))

## [0.8.2] — 2026-11-27

### Breaking

- trusty-browser 0.8.1 ships a public-API break under the internal numbering
  policy (docs/reference/semver-gate.md); recorded in `scripts/semver-accepted-breaks/trusty-browser-0.8.1.txt` (Refs #8699).

### Added

- `page_click`, `page_hover`, `page_type`, `page_press`, `page_select` and `page_scroll` act inside cross-origin child frames, addressed by the frame-scoped refs (`<frame>:<epoch>.<n>`) that `page_read` and `page_find` return. Tool names and inputs are unchanged. The action runs only in the child document the read recorded, under the lease's current top document, and only while the extension holds host permission for the child's own origin; otherwise it answers `DocumentChanged`, `StaleElement` or `PermissionNeeded` (with the child's `origin`) and never falls back to the top frame. A click stays trusted CDP input: the child's point maps through its owner iframe's transformed content quad from the top debugger session (no page-visible message places it) once that quad holds still across drawn frames, so a scroll the action itself causes cannot misdirect the press, an ambiguous, covered or perspective-transformed owner answers `Obscured`, and a click the child did not see arrive answers `OutcomeUnknown`. A `page_press` with no `ref` while focus is inside a cross-origin child answers `NotEditable`. The reply, and every refusal raised after the child resolved, adds `frame` (`id`, `origin`), and the `operation` audit line lists the child's host in `frames` for a success and a refusal alike ([#11808](https://github.com/bobmatnyc/trusty-tools/issues/11808)).

### Fixed

- `page_find` with only `role`, only `name`, or both no longer answers `InvalidRequest`; the broker refused the null `text` the MCP server sends for an unused term. A refused `page_find` now names the argument in `field` (Refs #11860).

## [0.8.1] — 2026-11-24

### Added

- `page_emulate` disables JavaScript, forces `prefers-color-scheme` light or dark, and sets the viewport size of a leased tab; a call with the defaults resets it. The emulation resets when the lease ends: release, expiry, session close, grant end, a dropped native port or a worker restart, and before a new lease's first probe; a reset the worker refuses is retried, and until it lands `select` refuses the tab with the retryable `EmulationResetPending` (#11371).

### Fixed

- `page_read` with `include_text` keeps up to half of `max_chars` for the page text, so a link-heavy page no longer returns empty text, and granted cross-origin child frames now contribute their text under a `[frame <id>]` marker (#11284).
- `trusty-browser install-service` restarts an already-loaded broker (`launchctl kickstart -k`), so an upgrade serves the new binary and extension asset stamp. A failed restart exits non-zero with the launchctl error.
- Grants survive a broker restart. The broker keeps `grants.json` in its state directory (0600, atomic write; grant id, scope, profile id and browser incarnation only, never a key or ticket) and restores it at start, still subject to the 45 s reconnect grace and the incarnation rule. A symlink, a non-owner-only mode, bad JSON or a malformed entry rejects the whole file and restores nothing. Every grant end rewrites the file, and a failed rewrite deletes it. When the rewrite and the delete both fail, the stale file may still name a revoked grant, so the broker refuses every new grant connect (`GrantStoreStale`) until a later persist succeeds; it retries at each grant end and at each connect attempt. Known gap: a broker restart while that stale file is still on disk can restore a revoked grant within the reconnect grace; a follow-up closes it.
- A stale extension worker recovers when its pairing handshake fails. The broker's refusal frame on `service.sock` now carries the published asset stamp, so the worker reloads on a mismatch. A handshake timeout, or a disconnect before the acknowledgement, shows "Pairing timed out" or "Pairing failed" in the popup status instead of failing silently.
- A broker restart no longer restores a grant that has a `grant_end` row in the audit log, so a stale grant store left by a failed rewrite and delete cannot bring back a revoked grant. An audit log that cannot be read restores no grants.

### Changed

- `peer`, `private`, `codesign`, `launchd`, `install_host` and `audit` now use the shared `trusty-daemon-common` crate for the peer uid read, owner-only file checks, signing, the plist writer, owned-file writes and audit rotation; public items keep their paths and signatures, and `write_owned` now fsyncs before linking (#11692).

## [0.8.0] — 2026-11-19

### Added

- `page_read` and `page_find` read cross-origin child iframes, read-only, when the extension holds host permission for the child's own origin and the tab is in the grant's scope. Results add a `frames` table and `denied_frames` (with a reason code), every entry names its `frame`, child refs are frame-scoped (`<frame>:<epoch>.<n>`) and answer `StaleElement` once the frame navigated, and the audit line lists the child hosts read ([#11301](https://github.com/bobmatnyc/trusty-tools/issues/11301))

## [0.7.0] — 2026-11-18

### Added

- `install-service` and `install-host` sign the installed `trusty-browser` binary in place with the team-4JH68XUHC5 Developer ID and identifier `com.trusty.browser` when it does not already satisfy that requirement, verify it, and write no plist or manifest if signing or verification fails
- `--sign-identity` and `TRUSTY_BROWSER_SIGN_IDENTITY` choose the identity; otherwise the one valid team-4JH68XUHC5 Developer ID Application identity is used, and none or several is a refusal
- `--no-sign` registers the binary unsigned, with a warning that every upgrade brings back the Keychain prompt
- Both install verbs also sign and verify the `trusty-browser-mcp` beside the installed `trusty-browser`, with the same identity and identifier, and register nothing if it fails; a missing `trusty-browser-mcp` is an error unless `--no-sign` is given
- An end-to-end smoke test drives the real `trusty-browser` and `trusty-browser-mcp` binaries in temp directories through serve, native-host pairing, connect, `tools/list`, one page op over a fake worker and a clean shutdown; a `test-seams` build of `serve` reads `TRUSTY_BROWSER_TEST_SEAMS=1` to use an in-memory key store and an open enrollment check, and a release build has no such switch
- New `trusty-browser` crate skeleton (0.7.0) with parity fixtures captured from the Python 0.7.0 broker and MCP server, and a test target that parses every fixture
- Library layer of the `trusty-browser` port: Python-exact JSON (`pyjson`), the length-framed codec, refusal types, the pairing HMAC proof, the `audit.jsonl` writer with 5 MiB rotation, the `registry.sqlite3` schema and pairing rows, and the AES-GCM vault with a pluggable Keychain store; parity tests compare each against the Python 0.7.0 fixtures byte for byte
- Broker of the `trusty-browser` port: the four Unix sockets with Python's paths, 0600 modes and `service.lock`, same-user peer authentication (`LOCAL_PEERCRED`/`SO_PEERCRED`) and the Darwin enrollment checks (libproc, `KERN_PROCARGS2`, live `codesign`), the pairing handshake, pair/unpair/connect/grants/revoke and operator ops, standing grants with the 45 s reconnect grace, and the `native-host` subcommand; replies, audit lines and registry rows replay byte-identically against the Python 0.7.0 capture
- Email case folding now matches Python `str.casefold` on every code point (full Unicode table generated from Python 3.12)
- Page ops of the `trusty-browser` port: tab leases with the 60 s TTL, the per-session request journal, and every agent op (`tabs`, `select`, each `page_*` op, `release`, `switch`) with Python's validation, result shapes, worker-refusal fate, `request_id` refusals, `TabClaimed` and `StaleLease`; the `lease_show`/`lease_clear` indicator command; a mutating op whose grant ends mid-flight answers a journaled `OutcomeUnknown`; replies and audit lines replay byte-identically against a new Python 0.7.0 page-op capture, including the asset stamp in the hello acknowledgement
- MCP server of the `trusty-browser` port: a `trusty-browser-mcp` binary serving stdio MCP in connect mode and workflow mode on the shared `trusty-mcp` envelope types, with static `tools/list` schemas, FastMCP's argument pre-parse and pydantic's lax coercion and error text, the `agent.sock` client with Python's frames and refusals (`NotConnected`, `AlreadyConnected`, `ServiceUnavailable`, `GrantEnded`), and the `--upload-root` checks with Python's MIME guesses; `initialize`, `tools/list`, every tool call and every frame replay byte-identically against a new Python 0.7.0 MCP capture
- Phase 4 of the `trusty-browser` port: the Chrome extension is embedded byte for byte (extension id `jabfhcolknbbdajilgpficniedhcfahc`) and `extension-path` publishes it into `state/extension` with Python's `version_name` stamp, file modes, stale-file pruning and `assets/<hash>` symlink migration; the published tree is byte-identical to Python 0.7.0's
- `install-host` writes Python's two Chrome native-messaging manifests and launchers, with the launchers running `trusty-browser native-host`; `launchd-plist`, `install-service` and `uninstall-service` manage the `com.trusty.browser` launchd job running `trusty-browser --state <dir> serve`, and accept the exact files Python 0.7.0 wrote so the cutover needs no hand edits
- The operator verbs `grants`, `revoke-grant`, `revoke-grants`, `pairings`, `unpair` and the legacy `revoke` with Python's frames, messages, output and exit codes
- `migrate-keychain --from-stdin` takes over the Keychain wrapping key: it reads the key from stdin only, proves it by unsealing every pairing, then deletes the old item, writes its own and reads it back

### Fixed

- The crate builds clean on Linux: the Darwin-only peer imports are gated with the code that uses them, and the launchd label comes from the shared `trusty_common::launchd_labels::BROWSER` registry entry (same `com.trusty.browser` value)
- A persistent accept error (EMFILE, ENFILE, ENOBUFS, ENOMEM) makes the broker wait 1 s before it accepts again, as Python asyncio does, instead of spinning
- `install-service` over a loaded Python broker job boots it out and bootstraps the new plist, so the Python broker no longer keeps running
- A `test-seams` build (any `cargo test` or `--all-targets` binary) refuses `install-host` and `install-service` with "test build; refusing to register", and its `serve` prints a stderr banner when the seams are active

### Security

- The enrollment check refuses unsafe Chrome switches given with one dash (`-load-extension=...`, `-user-data-dir=...`) as well as two, including after a bare `--` separator; `--remote-debugging-address`, `--enable-automation` and `--remote-allow-origins` are now unsafe too
- The native host re-reads the broker's peer PID after its executable and argv checks, closing a PID-reuse window
- The argv[0] pin compares strings before it canonicalizes a peer-chosen path
- The enrollment `HostCheck` seam and `BrokerOptions::host_check` exist only under the `test-seams` feature, so a release build cannot install an open check

### Documentation

- Rewrite the crate README for crates.io: summary, install, the consent and grant model, every MCP tool in both modes, a short Python cutover and a sourced comparison with Claude in Chrome, Playwright MCP, Browserbase/Stagehand, browser-use and Chrome DevTools MCP

