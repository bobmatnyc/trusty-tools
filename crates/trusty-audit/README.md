# trusty-audit

The auditor client. A client company receives it, runs it against their own
codebases, and returns a report. It installs the pinned tools it needs
(`tga`, `trusty-analyze`, `trusty-review`) and drives the audit workflow —
"really an installer runner".

**Status (#5502, #5495, #5555, #5499).** The crate, its working-directory
layout, the CLI surface, pinned tool installation, the audit run, and the return
package exist. Content signing and the desktop shell do not — they are later
milestones on #5473 / #5477. The run reads a selection file; its shape is
documented under "Running the sweep".

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

A bare invocation is the entry point, and on a terminal it is the whole
engagement: it asks for the audit targets one at a time, registers each as you
enter it, installs the pinned tools, and then asks before starting the sweep.

## Handing over the target list instead of typing it

Put a `repos.txt` or a `boards.txt` beside `engagement.toml` — the directory you
run from, or wherever `--config` points — and the launch reads it instead of
asking for each target. One per line; `#` starts a comment. Both short forms and
the URLs you copy out of a browser are accepted, and a `.git` suffix, a
`/tree/<branch>` path and a Linear issue number are all stripped:

```
# repos.txt
acme/api
https://github.com/acme/web.git
https://github.com/acme/iac/tree/main

# boards.txt
linear:ENG
https://linear.app/acme/team/PLATFORM/active
https://acme.atlassian.net/browse/OPS-412
```

One line that is not a target stops the whole read: nothing from either file is
registered, every bad line is named with its number and its own reason, and you
fix them and run again. Your OpenRouter key is already saved by then, so the
second run does not ask for it. Partial registration is what the rule exists to
prevent — an audit covering nineteen of twenty repositories still reports
success, over the one it never saw.

Either way the launch ends on the same review menu, which states how many
repositories it will clone and how many boards it will collect from, and offers
add, delete and proceed. Those counts are the point: `17 repositories, 2 boards`
catches a truncated file at a glance. Add and delete write through to
`engagement.toml`. With no terminal there is no menu — the counts and the full
list are printed and the run proceeds.

Two ways to get the status card instead, prompting for nothing: run
`trusty-audit guided`, which is the named verb for exactly that, or run the bare
invocation with no controlling terminal — a script, a cron entry, a CI job.
`TRUSTY_AUDIT_NO_LAUNCH=1` is not one of them: it is read by `install.sh`, where
it decides whether the installer starts the binary at all, and the binary itself
never reads it.

```
trusty-audit                    # guided flow: register targets, install, sweep
trusty-audit workdir            # create the working directory, print what lands where
trusty-audit add repo OWNER/NAME    # register a repository, after checking it can be read
trusty-audit add board jira:KEY     # register a JIRA project or Linear team
trusty-audit targets            # what this engagement is registered to audit
trusty-audit remove TARGET      # drop a registered target
trusty-audit repos              # repositories the companion manifest.toml records,
                                #   which a completed sweep writes — NOT the registry
trusty-audit tools              # which pinned tools are installed, at which versions
trusty-audit install            # download and verify the pinned tools
trusty-audit manifest           # engagement metadata from the companion manifest.toml
trusty-audit run                # run `tga audit` over the selected repositories,
                                #   resuming an interrupted sweep
trusty-audit run --fresh        # audit every selected repository again
trusty-audit package            # assemble the deliverable zip to send back
trusty-audit audit              # all four of the above, in one invocation
```

`targets` and `repos` answer different questions, and reaching for the wrong one
is the mistake worth naming. `targets` lists the REGISTRY — what `add` wrote, and
what the sweep will cover. `repos` reads the companion `manifest.toml`, which
`tga audit` writes once a sweep completes, so before any sweep it is empty no
matter how many targets are registered.

## The one-shot run

`trusty-audit audit` drives the whole engagement: it installs the pinned tools,
clones the repositories `trusty-audit add repo` registered, sweeps them, and
assembles the return package. It is the command an operator who has already
registered their targets runs; the four separate verbs stay for debugging one
phase at a time.

Interrupt it and run it again. Installed tools, complete checkouts and audited
repositories are all carried over — the same per-phase re-entrancy the separate
verbs have, chained.

It continues past a repository that fails, because one failure in six should not
discard five audits. What it will not do is let that read as a whole engagement:
the package names every repository it does not cover, the process exits non-zero
whenever anything registered was not audited, and a sweep in which NOTHING was
audited stops before the package phase rather than producing a zip of two
generated files. A failure names the phase it came from — install, materialize,
collect or package — so a stopped run says which step to look at.

A registered board (`jira:ACME`, `linear:ENG`) is reported as a stated gap
rather than collected. `tga audit` does take a JIRA project, but it reads that
credential as a literal string in its config file, and this client passes
secrets to a child through its environment and never through a file. Wiring
boards through needs env-var expansion on tga's JIRA credential first.

The same program is also installed as `taudit`, a shorter name for repeat use —
`taudit workdir` and `trusty-audit workdir` are one binary built from one
`src/main.rs` (the `trusty-installer` / `tctl` precedent).

Global options: `--work-dir <DIR>`, `--manifest <FILE>`, and `--config <FILE>`
(the engagement config, default `./engagement.toml`).

## Running the sweep

`trusty-audit run` audits each selected repository with the `tga` this client
installed and verified — never a `tga` on your `PATH`. If the pinned triple is
not installed the run refuses and says to install it; there is no fallback,
because an unpinned tool is the version skew #5454 cost us.

The repositories come from `<work-dir>/state/selected-repos.toml`. Selection and
cloning are separate work (#5487, #5215); until they land, write the file
yourself:

```toml
count = 1                   # how many entries follow — REQUIRED

[[repositories]]
name = "acme-api"
path = "repos/acme-api"     # relative paths anchor to the work-dir root
```

Write it to a temporary file in `state/` and rename it into place: a rename is
atomic, a write is not, and a producer that crashes part-way through one leaves
valid TOML holding a prefix of the entries. `count` is what makes that
detectable — a file carrying fewer entries than it declares is refused, not
treated as a smaller selection. Absent or empty is a refusal too, never a
zero-repository success.

One `tga audit` child runs per repository, so a failure is attributable to one
repository rather than to "the run". Files are stemmed `<index>-<name>` because
sanitizing alone is not injective — `acme/api` and `acme-api` would otherwise
share an output directory and a log file, and the second child would overwrite
the first's evidence. Each child writes to `out/<stem>/`, `logs/<stem>.log`, and
`extract/<stem>.db`. The results land in `state/run-progress.toml`.

**An interrupted run is resumed, not repeated.** `state/run-progress.toml` is
written after every repository — through a temporary file and a rename, so a
`kill -9` mid-write leaves either the previous whole record or the new one,
never a prefix. Re-run `trusty-audit run` and it picks up where it stopped: a
repository the record calls audited is carried over and printed as `resumed`, a
repository it calls FAILED is retried, and the summary separates the two. A
failure is usually the transient thing you re-ran to clear, so skipping it would
make the re-run a no-op reporting the same failure forever.

Being carried over is decided against the disk, not against the record alone.
The recorded output must still exist and still pass the same verification that
accepted it the first time, and the current selection must still put that
repository at the same `out/<stem>/` — reordering the selection changes the stem,
so a moved repository is audited again rather than credited with a directory this
run never wrote. Every re-collection states its reason. A record that exists but
cannot be parsed is a refusal, not an empty slate: starting over silently would
redo hours you were told were saved.

`trusty-audit run --fresh` ignores the record and audits everything again.
Reach for it when the recorded outputs are stale rather than missing — you
re-cloned the repositories, or changed the config in a way that should reach work
already done. It is the expensive direction, which is why it has to be asked for
by name.

`trusty-audit package` refuses a sweep that did not finish. The record carries a
completion flag, so a checkpoint left behind by a run that died is not mistaken
for a short run that succeeded, and a partial engagement is not sent as a whole
one. The remedy it names is the resume.

**A zero exit from `tga audit` is not proof anything was assessed.** Its own
contract is to exit 0 whenever the sweep completed, failed stages included — so
this client checks what the child produced: the manifest must exist, parse, name
a repository, and state no failed `collect` stage. A child that exits 0 having
written nothing is a failure. Other stated gaps (an unconfigured JIRA project, a
repository that could not be fetched) are DOC-67 §9 named gaps: they are printed
and recorded, and they do not fail the repository.

A child that outlives four hours is killed and recorded as a timeout, so a hang
costs one repository rather than the whole unattended run.

The exit status distinguishes the three outcomes DOC-67 §9 needs: 0 when every
repository was audited, 1 when only some were (`PARTIAL`, naming which failed),
and 1 when none were. A partial sweep never reads as a clean one.

The run uses the triple this client installed AND verified, at the version the
engagement config pins today — a config bumped after `install` refuses the run
rather than silently running the older binary.

**What the run does not pin.** `tga` runs from its absolute path, and the report
renderer, the search binary and the analyze binary are named to the child
through `TRUSTY_REVIEW_BIN`, `TRUSTY_SEARCH_BIN` and `TRUSTY_ANALYZE_BIN`, so
none of them can come from your `PATH`. What stays unpinned is the analyze
daemon's ADDRESS: `trusty-review report --analyze` reads metrics over HTTP from a
URL (default `http://127.0.0.1:7879`, overridable with `TRUSTY_ANALYZE_URL`), so
whatever is listening there is what answers.

**The code-analysis leg (#6081, #6082).** After each repository is audited, this
client indexes its checkout in `trusty-search` and measures it with
`trusty-analyze`, starting either daemon if it is not already answering, and
writes the resulting ranking into that repository's `manifest.toml` as
`inspect_priority`. That is what the report's investigation pass inspects first,
so the code it reads is the code a tool pointed at rather than the code whose
path name looked interesting. `trusty-audit render` does the same indexing and
measuring before it re-renders, because that path invokes `trusty-review report
--analyze` too.

The ranking has two sources. `trusty-analyze` names the complexity hotspots.
`trusty-search` answers a query set per due-diligence dimension — credential
handling, swallowed errors, lock and cache consistency, and the rest — plus
queries derived from the engagement's own `instructions` brief when it declares
one. The two are interleaved round-robin, so the report's inference budget is
spent across the dimensions rather than down whichever one complexity happens to
concentrate in. Each entry records which dimension it is evidence for and which
query found it:

```toml
inspect_priority = [
    "src/pay.rs",
    { path = "src/session.rs", dimension = "authentication & secrets", reason = "trusty-search hit for \"credential handling…\" (score 0.88, line 18)" },
]
```

The client also asks for a wider investigation budget than `trusty-review`'s own
default, by writing `investigate_max_files` / `investigate_max_bytes` into
`[report]` when the manifest declares neither — 120 files and 1.2 MiB per
repository, overridable per machine with `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES`
and `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES`. A manifest that already declares a
budget keeps it.

Every leg of that is fail-open: a daemon that will not start, a checkout
`trusty-search` refuses, an index that matches no evidence, an index with
nothing complex in it. None of them fails the repository, and none of them is
silent — each states one line naming the repository and what the report
therefore does not carry, both on the console and in the manifest's own gap
list, so the rendered report states it as well. The two ranking sources are
independent: a dead `trusty-analyze` costs the complexity ranking, not the
search-derived evidence.

The engagement's `instructions` prose and an audit window are NOT yet passed to
the child: `tga audit` takes `--weeks`, not free prose, and mapping one to the
other is its own decision. The instructions travel with the config for the human
reading it.

The engagement's OpenRouter key reaches the `tga` child through its
**environment** — `tga audit` spawns `trusty-review report`, which needs
inference. It is never written to a config file, a log line, or an error
message. The limit of that seam: a child's environment is readable by other
processes running as the same user on the same machine.

## The working directory — what is written where

Everything this client writes lives under one root. The default is
`./trusty-audit-work`, overridable with `--work-dir` or `$TRUSTY_AUDIT_WORKDIR`.

| Path | Contents |
|---|---|
| `tools/` | pinned `tga` / `trusty-analyze` / `trusty-review` binaries |
| `repos/` | **clones of your repositories — your source code** |
| `extract/` | the tga extract database, derived from those clones |
| `state/` | repository selection, tool versions, and the run checkpoint |
| `out/` | the deliverable to return: report and `manifest.toml` |
| `logs/` | output from the tools this client runs |

**Deleting it.** `rm -rf <work-dir>` removes everything this client wrote.
Nothing is written outside the root — that is a tested property
(`workdir::layout_tests::every_layout_path_is_inside_the_root`), not a claim.
Deleting mid-run loses the clones and the run progress; nothing else is affected.
Deleting only `state/run-progress.toml` throws away the resume — the next
`trusty-audit run` re-collects every selected repository from scratch.

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
trusty-search = "0.47.0"
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

## Sending the deliverable back

`trusty-audit package` turns what the sweep produced into one file:

```
trusty-audit package                              # <work-dir>/audit-return-package.zip
trusty-audit package --out ~/Desktop/return.zip   # wherever you will attach it from
```

The zip carries each audited repository's report directory (`reports/<repo>/`),
the tga extract database those reports were computed from (`extract/<repo>.db`),
and two generated files: a `README.md` explaining what is inside, and a
`package.toml` naming which repositories were covered and at which tool
versions. The last line of the output is the path to send.

**It is unencrypted and has no password, deliberately.** You can open it and
read exactly what you are about to send, which is the same premise as the
readable engagement config. Encrypting it would defend against nobody — you hold
the plaintext either way.

**What it will not send.** The engagement's OpenRouter key is never in it: every
member's bytes are scanned for the key while the zip is written, and a match
refuses the whole package rather than omitting one file. A symlink or a hardlink
under `out/` or `extract/` is refused for the same reason — either would put a
file from outside the working directory into an archive that leaves your
network. Refusals leave no zip and no partial file.

Hardlinks are checked by link count, because nothing else can see them: a
hardlink is a second directory entry on the same file, not a link *to* anything,
so it is indistinguishable from an ordinary file by type or by path. The cost is
that a legitimate file with more than one link is refused too. Nothing the audit
itself writes has one — freshly created files have a single link — so reaching
that state takes a deliberate `ln`, `cp -l`, or a hardlink-based backup tool
pointed into the working directory. If you hit it, copy the file instead of
linking it.

Two things it does not claim. The extract database holds no file content,
diffs, patches, hunks or blobs — but it does hold free-text fields (commit
messages, PR and work-item titles, classification notes), so a snippet someone
pasted into one of those is in it. And nothing here is signed yet: content
signing is #5481, and until it lands nothing proves the package was not altered
after it was written.

A sweep that audited nothing produces no package. A sweep that audited some
repositories does, and it names the ones it does not cover — in the printed
output, in `package.toml`, and in a non-zero exit status.

`--out` is the one path on which this client writes outside the working
directory, and only when you name one.

## Re-rendering a delivered audit

Whoever receives the finished audit can produce the report again from the
package itself. Unzip it, change into the directory that came out, and run:

```
trusty-audit render
```

That is the whole command. It reads the `engagement.toml` beside you for the
OpenRouter key, finds every `reports/<repo>/manifest.toml` under you, and runs
the report step over each one — the same step the sweep ran — writing the fresh
copies to `rerendered/` in that same directory. It clones nothing, collects
nothing, and never writes into the package it read, so the delivered files stay
byte for byte what was sent.

Every default is overridable, and no flag is required:

| Flag | Default |
|---|---|
| `--config` | `engagement.toml` in the directory you are in |
| `--from` | the first of these that holds reports: the directory you are in, then `work/` beside your `engagement.toml`, then the work dir |
| `--out` | `<from>/rerendered` |
| `--review-bin` | the copy under `<work-dir>/tools/`, else whatever is on `PATH` |

```
trusty-audit render --from ~/acme-audit --out ~/tmp/second-opinion
```

The middle rung of `--from` is for the operator who ran the engagement rather
than received it. Sweeps run with `--work-dir ~/acme/work` leave their reports
under `~/acme/work/out/`, which is neither the directory holding
`engagement.toml` nor the default work dir — so a bare `trusty-audit render`
from beside the config finds them. Nothing in `engagement.toml` declares a work
dir; `work/` beside it is the layout the flow produces, and `--work-dir` or
`--from` names anywhere else. When none of the three holds a report, the refusal
names all three.

**The renderer says where it came from.** There is no pin check, so a
`trusty-review` picked up off `PATH` renders at whatever version it happens to
be. When that is what happened — no `--review-bin`, and no copy under
`<work-dir>/tools/` — the run prints one line naming the resolved path and the
version it answered, before the reports. A renderer you named, or one this
engagement installed, prints nothing: the line means something only because it
is not on every run.

The key can come from the environment instead of the config, which is what a
recipient who was sent a key rather than a package config uses:

```
export OPENROUTER_API_KEY=<your OpenRouter key>
trusty-audit render
```

With neither, the run refuses and names both places it looked. It needs
`trusty-review` on the machine; there is no pin check — the verb renders at
whatever version it finds.

**What comes back, and what does not.** The scorecards, findings and appendices
rebuild from the collected data that shipped in the package. Two things differ
from the copy you were sent:

- The executive summary and top risks are written by a model, so they are worded
  differently over the same figures.
- The code scan and the analysis pass need the repositories themselves, and the
  package carries none — those repositories are named as gaps, one line each,
  rather than left out silently.

A report that fails to render is named with its log path and the others still
render; a run that could not regenerate everything it found exits non-zero.

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
