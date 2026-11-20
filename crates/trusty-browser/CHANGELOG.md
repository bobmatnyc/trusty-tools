# Changelog — trusty-browser

All notable changes to trusty-browser are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

Unreleased changes live as fragments in `changelog.d/`.

---

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

