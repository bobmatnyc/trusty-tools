# trusty-audit

The auditor client. A client company receives it, runs it against their own
codebases, and returns a report. It installs the pinned tools it needs
(`tga`, `trusty-analyze`, `trusty-review`) and drives the audit workflow —
"really an installer runner".

**Status: scaffold (#5502).** The crate, its working-directory layout, and the
CLI surface exist. The audit run, package assembly, signing and the desktop
shell do not — they are later milestones on #5473 / #5477.

## Shape: a library with a CLI over it

Two constraints are permanent, not phases:

- **Headless first.** The desktop shell is a later milestone and will be a view
  over the same API the CLI calls.
- **Every feature is CLI-testable, forever.** For any capability there is a CLI
  invocation that exercises it end to end with no window. This is enforced by
  the code, not by review: `session::Command` is the capability set,
  `Session::execute` is the only way to run one, and `cli.rs` matches
  exhaustively over `Command` — a capability with no CLI path does not compile.

## Running it

A bare invocation is the entry point. It starts the guided flow, which walks the
pre-run steps in order (repository selection first, then tooling); it does not
launch an unattended sweep.

```
trusty-audit                    # guided flow
trusty-audit workdir            # create the working directory, print what lands where
trusty-audit repos              # repositories this engagement is configured to audit
trusty-audit tools              # which pinned tools are installed
trusty-audit manifest           # engagement metadata from the companion manifest.toml
```

The same program is also installed as `taudit`, a shorter name for repeat use —
`taudit workdir` and `trusty-audit workdir` are one binary built from one
`src/main.rs` (the `trusty-installer` / `tctl` precedent).

Global options: `--work-dir <DIR>` and `--manifest <FILE>`.

## The working directory — what is written where

Everything this client writes lives under one root. The default is
`./trusty-audit-work`, overridable with `--work-dir` or `$TRUSTY_AUDIT_WORKDIR`.

| Path | Contents |
|---|---|
| `tools/` | pinned `tga` / `trusty-analyze` / `trusty-review` binaries |
| `repos/` | **clones of your repositories — your source code** |
| `extract/` | the tga extract database, derived from those clones |
| `state/` | repository selection and run progress |
| `out/` | the deliverable to return: report and `manifest.toml` |
| `logs/` | output from the tools this client runs |

**Deleting it.** `rm -rf <work-dir>` removes everything this client wrote.
Nothing is written outside the root — that is a tested property
(`workdir::layout_tests::every_layout_path_is_inside_the_root`), not a claim.
Deleting mid-run loses the clones and the run progress; nothing else is affected.

**Open questions, recorded rather than resolved** (#5502): whether the root
should sit beside the unzipped package (today's default) or under the user's
home directory; whether two runs may share one root (nothing locks today); and
whether a `clean` verb should exist. See the `workdir` module docs.

## Credentials

The engagement config that arrives with the package carries a spend-capped
OpenRouter key and the audit instructions. **The deliverable that goes back
carries no key.** `config::SecretKey` implements `Deserialize` and deliberately
not `Serialize`, so writing the key into an output artifact is a compile error;
its `Debug` and `Display` both redact, so it cannot reach a log line or an error
message either.

## Tool downloads

`tools::install` is a **seam**, not an implementation. Downloading, verifying
and placing binaries is `trusty-installer`'s domain, and #5491 is adding the
pinned, fail-closed entry point this crate will call. Until then the seam
returns an error rather than falling back to anything — a bespoke download here
would duplicate that domain, which CLAUDE.md's common-entry-point rule forbids.

## Reading the manifest

`tga audit` writes `manifest.toml` into its output directory (DOC-67 §6):
engagement metadata plus one `[[repositories]]` entry per configured repo. This
crate reads that file rather than keeping a second copy of the same facts. Note
what it does **not** carry: it is written once, after the sweep completes, and
records the *configured* repository set — not per-repo or per-phase completion.
Run progress is a separate record this crate will own (#5494).

## Development

```bash
cargo check   -p trusty-audit --all-targets
cargo clippy  -p trusty-audit --all-targets -- -D warnings
cargo test    -p trusty-audit
```

Note `-p trusty-audit` is the Cargo package name; `trusty-audit` and `taudit`
are its two binary targets.
