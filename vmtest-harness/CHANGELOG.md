# Changelog — `vmtest-harness`

`vmtest-harness` is a shell component, not a published crate: it has no version, no
release tag, and no `changelog.d/` fragment directory (the per-PR fragment gate covers
`crates/<crate>/src/**` only). Changes are recorded here, newest first.

## [Unreleased]

### Added

- `vmtest run branch` now performs an **authenticated** guest-side `git clone` when the
  host has a `GITHUB_TOKEN`, instead of an anonymous one that shares a single github.com
  rate-limit quota with the host and every concurrent guest on the same egress IP
  (closes [#4924](https://github.com/bobmatnyc/trusty-tools/issues/4924)). The token is
  read from the host process environment, crosses to the guest **on stdin only**, and is
  written to a `0600` git-config include created under `umask 077`; it is never a config
  key, never in `repo_url`, never in `$VMTEST_GUEST_ENV`, and never in host argv. Only
  its presence and the pass/fail outcome are logged. The wiring is
  `http.https://github.com/.extraheader` — sent preemptively — because a credential
  helper is consulted only after a 401 and github.com serves this public repository with
  200, so a helper is never invoked at all. The guest's inherited interactive
  credential-helper chain is cleared first, without which a rejected credential hangs
  `git ls-remote` in the headless guest. An in-guest `git ls-remote` proves the
  credential end-to-end over the network rather than echoing configuration back.
- `propagate_github_token` — a new `vmtest.defaults` boolean (default `true`,
  overridable with `VMTEST_PROPAGATE_GITHUB_TOKEN`) gating the above. The boolean is
  banner-safe; the secret it gates is not, which is why only the boolean is a key.
- `preflight_config()` — validates that boolean **before any VM is cloned**, so a typo'd
  override is exit `10` in a second rather than after a 30 s+ boot, and `--dry-run`
  catches it too. Deliberately not folded into `conf_load()`, which `vmtest clean`
  shares: a typo'd override must never be able to break cleanup.
- **`--keep` now warns, at teardown, that the preserved VM retains the token**, naming
  the include file's guest path, that base64 is encoding rather than encryption, the
  `vmtest clean --include-kept` remedy, and that a shared or copied VM means the token
  should be revoked. Previously that caveat existed only in `README.md`, which an
  operator watching a `--keep` run finish is not reading. It fires only when a
  credential was actually written into the guest, so a no-token run and every
  `local` / `released` run stay silent.

### Fixed

- A failing `git clone` under pattern (b) no longer asserts flatly that the failure
  "is not a credentials failure". That was true while the clone was always anonymous;
  with a preemptive Authorization header, a token revoked between the `ls-remote` proof
  and the clone lands exactly there. The message now says an anonymous clone should have
  succeeded, that propagation makes a credential failure possible, and that
  `VMTEST_PROPAGATE_GITHUB_TOKEN=false` isolates it.
- The `credential.helper` reset and the `include.path` wire-in no longer discard git's
  stderr. The reset is the step whose anticipated failure is a specific git diagnostic
  (`cannot overwrite multiple values with a single value`, exit 5), so discarding it left
  the one predicted failure dying with harness prose and no evidence. Both now capture to
  a log that is echoed before the `die`.

### Notes

- An absent `GITHUB_TOKEN` is **not an error** — the run clones anonymously and exits 0
  exactly as before.
- `vmtest run local` and `vmtest run released` never receive the token and cannot fail
  because of it; the pattern gate sits above every branch that can fail the run.
- `--keep` leaves the credential include file on disk in the stopped VM. Base64 is
  encoding, not encryption. Remove it with `vmtest clean --include-kept`, and revoke the
  token if the kept VM was shared or copied.

## 2026-08-04

- Initial harness: VM-isolated install testing for all three install patterns
  ([#4773](https://github.com/bobmatnyc/trusty-tools/issues/4773)).
