# trusty-audit

The auditor client. A client company receives it, runs it against their own
codebases, and returns a report. It installs the pinned tools it needs
(`tga`, `trusty-analyze`, `trusty-review`) and drives the audit workflow —
"really an installer runner".

**Status (#5502, #5495).** The crate, its working-directory layout, the CLI
surface, and pinned tool installation exist. The audit run, package assembly,
signing and the desktop shell do not — they are later milestones on
#5473 / #5477.

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
trusty-audit tools              # which pinned tools are installed, at which versions
trusty-audit install            # download and verify the pinned tools
trusty-audit manifest           # engagement metadata from the companion manifest.toml
```

The same program is also installed as `taudit`, a shorter name for repeat use —
`taudit workdir` and `trusty-audit workdir` are one binary built from one
`src/main.rs` (the `trusty-installer` / `tctl` precedent).

Global options: `--work-dir <DIR>`, `--manifest <FILE>`, and `--config <FILE>`
(the engagement config, default `./engagement.toml`).

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

`trusty-audit install` fetches `tga`, `trusty-analyze` and `trusty-review` at
the exact versions the engagement config pins, and installs all three or none.
There is no download, checksum or extraction code in this crate: it calls
`trusty_installer::download::pinned::install_pinned_set`, which owns that domain
(#5491, #5495). `cargo install` is unreachable from that path, so a failure is
never a locally-built substitute.

The versions come from the engagement config, which is required — there is no
"latest" and no default:

```toml
[tools]
tga = "2.9.4"
trusty-analyze = "0.9.2"
# Pin the artifact's bytes as well as its version, when the handoff was built
# with a recorded digest.
trusty-review = { version = "0.15.1", sha256 = "9f86d0…" }
```

All three keys are required. A config pinning two of them does not parse, which
is deliberate: an unpinned tool is one that would resolve to whatever is
current, and #5454 is what a mismatched triple costs — a new `tga` paired with
an old `trusty-review` produced a deterministic report and exited 0.

**What it refuses.** A checksum that does not match, an artifact that cannot be
downloaded, a binary reporting a version other than the pin, a version that was
never published, a `tools/` directory that is really a symlink pointing outside
the working directory. Each installs nothing and exits non-zero. Whether the
install directory is clean is stated by the error itself — `trusty-installer`
distinguishes "nothing was placed" from an interrupted commit that left files,
and this crate passes that distinction through rather than flattening it.

**What it records.** On success, `state/tool-versions.toml` holds the exact
triple, and `trusty-audit tools` reports it. A binary sitting in `tools/` that
this client did not place shows as `UNVERIFIED` with no version, because a
version it did not verify is one it cannot vouch for. The deliverable package
(#5499) reads that record; it does not re-derive versions by running the tools.

### Known v1 risk: a network that blocks binary downloads

A recipient behind an egress proxy that blocks arbitrary binary downloads fails
at `trusty-audit install` — in the first thirty seconds, naming the URL it could
not reach — rather than an hour into an audit. That is as far as v1 goes.
Nothing retries through a proxy, and there is no bundled-binary fallback
variant; whether one should exist is deferred (#5495), not solved. The remedy
today is to allow the GitHub release-asset host, or to ask for a package built
for that network.

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
cargo test    -p trusty-audit -- --include-ignored   # adds the network refusals
```

The `#[ignore]`d tests reach the GitHub release API and are the only ones that
do; everything else runs offline.

Note `-p trusty-audit` is the Cargo package name; `trusty-audit` and `taudit`
are its two binary targets.
