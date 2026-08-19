# Changelog — trusty-audit

All notable changes to trusty-audit are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

Entries are assembled at release time from `changelog.d/` fragments — never
edit this file by hand (see
[docs/reference/changelog-fragments.md](../../docs/reference/changelog-fragments.md)).

---

## [0.6.0] — 2026-08-19

### Added

- `trusty-audit clone <owner/name>...` acquires repositories the recipient has
  never checked out, into the working directory's `repos/` area — via
  `gh repo clone`, reusing the credential `gh auth login` already resolved. A
  clone is built under `state/clone-staging/` and renamed into place only after
  `gh` exits zero AND the tree verifies as a real checkout, so neither an
  interrupted run nor a commitless repository (which clones with exit 0 and no
  commits) is ever promoted as whole. One repository failing is a named gap and
  the run continues, exiting 2 so a shell chain does not proceed against an
  incomplete set; only every repository failing aborts. Clones are shallow by
  default and disk use is reported; `--budget-gb` stops new clones from
  STARTING once that much is on disk and does not cap a clone already running
  (#5215, DOC-68 §8 / §14 Q2).
- The auditor client has a desktop shell: `trusty-audit-ui`, a Tauri 2 window at `crates/trusty-audit/ui`. Phase 1 shows one view — the working-directory root, the engagement's manifest state, per-tool install status, and the next step — and adds no capability ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- The shell calls `Session::execute` in-process through a Tauri IPC command. There is no HTTP endpoint and no `taudit` subprocess, so a capability still has exactly one implementation and one CLI arm, per DOC-68 §11 ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- The window distinguishes a tool it installed and verified from one that is merely present, the same three states the CLI prints as `ok` / `UNVERIFIED` / `MISSING`. Wording is the front end's; the states come from `Session::execute` ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- A failed guided call shows its reason and a retry, rather than an empty panel that would read as an engagement with nothing to report ([#5477](https://github.com/bobmatnyc/trusty-tools/issues/5477))
- `trusty-audit discover` lists every repository the recipient's `gh` credential
  can reach — their own repositories plus each organization the account belongs
  to, not one named org. Every `gh` call routes through `trusty-common`'s single
  entry point, and every failure is a refusal naming the owner that could not be
  listed, never a silently shorter list (#5487, DOC-68 §6 / §14 Q4).
- `trusty-audit run` resumes an interrupted sweep instead of repeating it.
  `state/run-progress.toml` is now written after every repository rather than
  once at the end, so a crash, a timeout or a Ctrl-C costs the repositories
  still to come and none of the ones already audited
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- Each carried-over repository is named in the run's output as `resumed`, and
  the summary separates what an earlier run audited from what ran now. A
  repository being re-collected states why — its output was deleted, the
  earlier run recorded it as failed, or `--fresh` was asked for
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit run --fresh` audits every selected repository again, ignoring
  the record. Reach for it when the recorded outputs are stale rather than
  missing — re-cloned repositories, or a config change that should reach work
  already done ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- A repository the record calls audited is carried over only if its output is
  still on disk and still passes the same verification that accepted it the
  first time, and only if the current selection puts it at the same output
  path. Anything else is audited again
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit package` refuses a sweep that did not finish, naming the count
  recorded so far and pointing at the resume. Packaging one would have sent a
  partial engagement as a whole one
  ([#5494](https://github.com/bobmatnyc/trusty-tools/issues/5494))
- `trusty-audit install` downloads and verifies the pinned `tga` / `trusty-analyze` / `trusty-review` triple, installing all three or none. It calls `trusty-installer`'s pinned, fail-closed entry point rather than implementing downloading a second time, so `cargo install` is unreachable and a checksum mismatch, an unpublished version, an unreachable host or a binary reporting the wrong version each install nothing ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- The engagement config carries the exact version triple under `[tools]`, as a bare version (`tga = "2.9.4"`) or with the artifact digest pinned too (`{ version = "…", sha256 = "…" }`). All three keys are required: a partly-pinned config does not parse, because an unpinned tool is one that resolves to whatever is current — the version-skew defect [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454) closed from the run side ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `state/tool-versions.toml` records the exact versions a successful install placed, so the deliverable can state which triple produced it ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- Global `--config <FILE>` names the engagement config that arrived with the handoff package; it defaults to `./engagement.toml` ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `trusty-audit package` assembles the deliverable to send back: one unencrypted zip carrying each audited repository's report directory, its tga extract database, and a generated `README.md` and `package.toml` stating which repositories were covered and at which tool versions. The path is printed as the last line, and `--out` writes it wherever the recipient will attach it from ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- The package refuses rather than ships when a member carries the engagement OpenRouter key, or when a symlink or hardlink under `out/` or `extract/` would pull in a file from outside the working directory. Hardlinks are caught by link count, since a hardlink is a second directory entry on the same file and no path- or type-based check can see one. Refusals leave no zip and no partial file behind ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- A sweep that audited nothing has no package, and a partial sweep's package names every repository it does not cover — in the returned value, in the printed output, in `package.toml`, and in a non-zero exit ([#5499](https://github.com/bobmatnyc/trusty-tools/issues/5499))
- New crate: the auditor client, a headless library with a thin CLI over it. Scaffold only — the capability set (`session::Command`), the working-directory layout (`workdir`), the engagement config and its non-serializable `SecretKey`, a reader for tga's `manifest.toml`, and the tool-install seam that fails closed pending `trusty-installer`'s pinned entry point. A bare invocation enters the guided flow. Every capability has a CLI invocation, enforced by an exhaustive match rather than by review ([#5502](https://github.com/bobmatnyc/trusty-tools/issues/5502))
- The binary installs under both names: `trusty-audit`, which the docs and the handoff README use, and `taudit` for repeat use. Two `[[bin]]` targets over one `src/main.rs` — the same arrangement as `trusty-installer` / `tctl` — so there is one implementation and no second code path to drift ([#5502](https://github.com/bobmatnyc/trusty-tools/issues/5502))
- `trusty-audit run` drives the audit sweep: one `tga audit` child per selected repository, using the pinned `tga` this client installed and verified rather than whatever is on `PATH`. A missing or unverified triple refuses the run and names what to install — there is no fallback, because an unpinned tool is the version skew [#5454](https://github.com/bobmatnyc/trusty-tools/issues/5454) closed ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- Per-repository results are captured, not just the sweep's overall exit code. A partial failure (one repository fails, others succeed) reads as `PARTIAL` and names which failed; a total failure says no repository was audited. Both exit non-zero, so a partial sweep can never be reported as a clean one ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/selected-repos.toml` is the run's input contract — `[[repositories]]` entries of `name` and `path`, with relative paths anchored to the work-dir root. Repository selection and cloning ([#5487](https://github.com/bobmatnyc/trusty-tools/issues/5487), [#5215](https://github.com/bobmatnyc/trusty-tools/issues/5215)) write this file; absent or empty is a refusal, not a zero-repository success ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/run-progress.toml` records what the last sweep did per repository. It is written after every child finishes, and a failure to write it fails the call rather than leaving a run that cannot be described ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `tga` is run from its absolute path and the report renderer is named to the child through `TRUSTY_REVIEW_BIN`, so neither comes from the operator's `PATH`. The analyze step cannot be pinned this way — `tga audit` invokes `trusty-review report --analyze`, which reads metrics over HTTP from a URL rather than spawning a binary — so `TRUSTY_ANALYZE_BIN` is deliberately left unset instead of set inertly ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- The engagement's OpenRouter key reaches the `tga` child by environment only — never a config file, a log line, or an error message ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- A zero exit from `tga audit` is not taken as proof anything was assessed. That command exits 0 whenever the sweep completed, failed stages included, so the client checks what the child produced: the manifest must exist, parse, name a repository, and state no failed `collect` stage. Other stated gaps are recorded and printed without failing the repository, per DOC-67 §9 ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- Per-repository files are stemmed `<index>-<name>`, so two repositories whose names sanitize alike — `acme/api` and `acme-api`, or `Acme` and `acme` on a case-insensitive filesystem — no longer share an output directory, a log file, or a database ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- The recorded tool versions are compared against the engagement's pins at run time, not only at install time. A config bumped between the two refuses the run instead of executing the older binary ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- A `tga audit` child that outlives four hours is killed and recorded as a timeout, so a hang costs one repository rather than an unattended run that leaves no record at all ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `state/selected-repos.toml` carries a required `count`, and a file holding fewer entries than it declares is refused — the partial write a producer crashing mid-write leaves behind, which otherwise reads as a smaller-but-complete selection. Writers must write a temporary file and rename it into place ([#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555))
- `run::save_selection` is the one writer of `state/selected-repos.toml`, honouring the two obligations that file's contract states: `count` ahead of the entries, and a uniquely-named temporary file renamed into place. The picker ([#5497](https://github.com/bobmatnyc/trusty-tools/issues/5497)) writes through it too, rather than re-deriving them ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- `tests/cli_end_to_end.rs` drives the whole chain through the real binary — several repositories selected, cloned and audited, with a stub `gh` and stub pinned tools and no network. Partial failure is exercised at both stages: a repository that cannot be cloned never reaches the sweep, and a repository that fails `tga audit` leaves the sweep `PARTIAL` and the process exiting non-zero ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- `trusty-search` is now a pinned tool alongside `tga`, `trusty-analyze` and
  `trusty-review`. `[tools]` in the engagement config gains a required
  `trusty-search` key, and `trusty-audit install` fetches the binary into
  `work/tools/`.
- `trusty-audit run` sets `TRUSTY_SEARCH_BIN` on every `tga audit` child, so the
  audit's search preflight starts the engagement's pinned copy instead of falling
  through to a PATH lookup on a clean machine.
- `--no-install` keeps the guided flow and `trusty-audit run` off the network,
  restoring the previous refuse-and-report behaviour exactly. `trusty-audit
  tools` never installs, with or without the flag, so a status read costs no
  download ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- `tools::unsatisfied` decides whether anything needs downloading from the same
  three conditions the sweep's preflight refuses on — the file is present, the
  version record names it, and that version equals the pin. An already-satisfied
  set constructs no HTTP client, so repeated invocations re-download nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- The guided flow and the `add` / `targets` help now coach registration breadth
  rather than waiting to be asked: applications, the repository holding the
  database schema or migrations, infrastructure and IaC, shared libraries and
  config repositories, and every ticketing board in use. The wording names what
  the assessment judges — how mature, how stable and how supportable the
  technology is — so the ask reads as audit quality rather than an inventory
  chore, and it claims no detection, because this client cannot see a target
  that was never registered. One `registry::COVERAGE_COACHING` constant, so the
  CLI and a later chat wizard present the same substance (#5822).
- `trusty-audit add repo <owner>/<name>` and `trusty-audit add board <jira|linear>:<KEY>` register what an engagement audits, additively — a second `add` never disturbs the targets already registered, and re-adding one is a no-op rather than an error ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- Each `add` proves the target can be READ before it is persisted, with the credential the sweep will later use: `gh auth token` then `gh repo view` for a repository, `GET /rest/api/3/project/<KEY>` for JIRA, a GraphQL team read for Linear. A target that cannot be reached is refused at registration, so it is not discovered as a gap an hour into an unattended sweep ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `trusty-audit targets` lists the registry and `trusty-audit remove <target>` drops one entry. Both accept `owner/name` or `provider:key`, and matching ignores ASCII case because GitHub, JIRA and Linear all do ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- The engagement config gained a `[boards]` table carrying the CLIENT's own JIRA (`url` / `email` / `token`) and Linear (`api_key`) credentials, as `SecretKey` values — no `Serialize`, redacting `Debug` and `Display`, the same rules the OpenRouter key has carried since [#5473](https://github.com/bobmatnyc/trusty-tools/issues/5473). Registering a board whose provider has no credential names the config field to set instead of returning an HTTP 401 ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `state/audit-targets.toml` is the registry file. It supersedes `state/selected-repos.toml` as the record of what the engagement TARGETS — it holds boards, which that file cannot express, and it exists before any clone. The selection file is unchanged and still what `trusty-audit run` reads as the record of what is on disk; `trusty-audit targets` names both so neither reads as lost ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- `taudit install`, `taudit clone` and `taudit run` show live progress instead
  of waiting silently. The sweep reports the stages `tga audit` is actually
  running, relayed out of each child rather than swallowed into its log file —
  a sweep that used to show nothing for up to four hours per repository now
  names the stage in flight ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- `Session::with_progress` takes the sink a front end renders through, so
  `Session::execute` still runs with no terminal and the Tauri shell renders the
  same updates its own way. Absent, every update is discarded and the
  capabilities behave exactly as before
- Off a terminal — CI, a pipe, a captured run — the display degrades to one
  plain line per state change: no escape sequences, no repainting, and no line
  for a mid-flight counter
- `trusty-audit audit` runs a whole engagement in one invocation — install the
  pinned tools, clone the registered repository targets, sweep them with `tga
  audit`, and assemble the return package — instead of `install`, `clone`, `run`
  and `package` in order. The four separate verbs are unchanged.
- The chained run is resumable: interrupting it and running it again carries
  over installed tools, complete checkouts and audited repositories.
- A phase that fails names the phase it failed in — install, materialize,
  collect or package — rather than reporting "the audit failed".
- A sweep that audited nothing stops before packaging, so a failed collection
  cannot produce a zip that looks like a finished engagement. A sweep that
  partly failed still packages, names what it omits, and exits non-zero.
- `trusty-audit distribute` builds the install package that goes TO a client: one zip at `~/duetto/audit` (or `--out <dir>`) holding the `taudit` binary, an `audit.sh` launcher that runs it from wherever it was extracted, a generated `engagement.toml` carrying the OpenRouter key, and a README naming the three commands in order. The recipient extracts it and runs a script — no Rust toolchain, no `cargo install`, no PATH entry. The key comes from `OPENROUTER_API_KEY` when set and from the template config otherwise; there is deliberately no flag for it, because argv is visible through `ps` and lands in shell history ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- `config::generate` renders an engagement config carrying a credential — the one narrow path that writes a `SecretKey` into a file, kept beside the type whose missing `Serialize` makes every other path a compile error. The outbound return package's credential refusal is unchanged and unreachable from it: the two directions are two modules producing two types, never one function with a flag ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- A registered board is now collected instead of reported as a gap. `jira:ACME`
  or `linear:ENG` becomes a `jira:` / `linear:` section on each generated tga
  config, so `tga audit` syncs the board alongside the repositories. Registering
  a board previously stated it as a gap and held the whole engagement at a
  non-zero exit.
- `trusty-audit run` and `trusty-audit audit` ask for the OpenRouter key when nothing else supplies one, instead of refusing until someone hand-edits `engagement.toml`. The key is typed with the terminal's echo disabled, confirmed by a retype, and saved back to the engagement config at mode 0600. The prompt accepts either the key an auditor conveyed out of band or the client's own OpenRouter key, and reports which source a run used without ever printing the value ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `Session::with_credential` lets a front end hand in a resolved credential. The prompt lives in the CLI, so `Session::execute` stays callable by a front end with no terminal — the Tauri shell today, a TUI next ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `curl -fsSL <url>/crates/trusty-audit/install.sh | sh` installs and launches `trusty-audit` on macOS in one command. The script resolves a release (or an exact `TRUSTY_AUDIT_VERSION`), downloads the tarball and its published `.sha256` sidecar, refuses to continue on a digest mismatch, proves the downloaded binary reports a version before anything reaches `PATH`, installs into `${CARGO_HOME:-$HOME/.cargo}/bin` with an atomic rename rather than `cp`, and launches `trusty-audit` with stdin attached to `/dev/tty` so its credential prompt still works under `curl | sh` ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- The script is a bootstrap, not a second installer: its only job is getting the binary onto the machine. It checks only what gates whether the binary can run at all — a supported macOS architecture, the tools the script itself uses, the checksum, and that the binary executes. Provider reachability, the credential, and the collection dependencies (`gh`, JIRA, Linear) stay with `trusty-audit`, which already owns tool installation through `install_pinned_set` and can check each one at the point it needs it ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- An unsupported host is refused before any network call. Intel Macs are named explicitly: no `x86_64-apple-darwin` asset is published for any crate in this workspace, so the script says so rather than downloading an arm64 binary that cannot execute ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- Tagging `trusty-audit-v<version>` now publishes the `trusty-audit` and `taudit` binaries. The crate was absent from the release workflow's crate allowlist, so such a tag previously green-skipped and published nothing — the "a missing binary release is silent" caveat that allowlist documents ([#5870](https://github.com/bobmatnyc/trusty-tools/issues/5870))
- Launching on a terminal now walks the operator through registration instead of printing "Next: register the repositories and boards to audit (`trusty-audit add`)" and exiting. It asks for one target at a time, registers each through the same validation `trusty-audit add` runs, shows the running list as entries land, and on an empty line carries on into tool installation and — after asking — the sweep, all in the one invocation. A refused target is reported and the prompt returns; only the terminal itself failing ends the session.
- With no controlling terminal the launch is unchanged: it prints the status card and prompts for nothing, so scripts and CI keep the shape they had.
- Targets registered with `add` now advance the guided flow. It read only the companion `manifest.toml`, which `tga audit` writes after a sweep completes, so it kept telling an operator to register what they had just registered.
- The pins a cold start records come from the latest published stable release of
  each tool, resolved once and written into `engagement.toml`. Every run after
  the first reads the file, so `latest` never reaches a second run and the
  engagement states the exact triple it ran
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A synthesised `engagement.toml` names its `[models]` table in full: OpenRouter
  as the provider, `anthropic/claude-opus-4.8` for the judging call, and
  `anthropic/claude-haiku-4.5` for the verifier and summarizer. Leaving the table
  out fell through to `trusty-review`'s own defaults, whose provider is Bedrock —
  an account this engagement never named. All four fields are written because the
  table is all-or-none, and because it sits above the built-in constants in
  `trusty-review`'s precedence chain, these are the models the audit actually runs
  on ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A `repos.txt` or `boards.txt` beside `engagement.toml` is the engagement's target list, and the per-target prompt loop is skipped rather than seeded. Lines take short forms and browser URLs — `acme/api`, `https://github.com/acme/api/tree/main`, `linear:ENG`, `https://linear.app/<workspace>/team/ENG/active`, `https://acme.atlassian.net/browse/OPS-412` — and a `.git` suffix is stripped rather than becoming the repository's name, which `clone::split_name` would otherwise accept as the literal name `api.git`. A Linear URL yields the team key, never the workspace slug. Every target is registered through `Command::AddTarget`, so a file-detected one is validated and declared in `engagement.toml` exactly like a typed one ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- One unparseable line refuses the whole read: nothing from either file is registered, every bad line is named with its number and its own reason, and the operator is told their OpenRouter key is saved so the re-run does not ask for it. This holds with and without a terminal — an audit that silently covers fewer repositories than the file lists reports success over the absent ones ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- A review menu now ends registration, reached from both the targets file and the prompt loop: it states how many repositories will be cloned and how many boards collected from, then offers add, delete and proceed. Add and delete write through to `engagement.toml`. The menu choice discards typeahead before it is read, because a queued keystroke that can select `delete` is worse than one that can answer a yes/no. With no terminal there is no menu — the counts and the full list are printed and the run proceeds ([#5978](https://github.com/bobmatnyc/trusty-tools/issues/5978))
- Every registered repository now automatically contributes its own GitHub
  issues to the sweep's ticketing correlation — no separate board
  registration for a repo's own tracker. The generated `tga` config for each
  repository always carries a `github:` section naming that repository, using
  the recipient's own `gh auth token` credential (never a new raw-token config
  field) when one can be read, and running unauthenticated otherwise rather
  than silently omitting the section. A repository whose issues are disabled,
  whose credential is rejected, or whose repo is invisible to the resolved
  credential (a private repo with no or an invalid token) is reported the
  same way an unreachable JIRA project already is — a named gap on the
  affected repository, not an empty-but-successful ticketing section (#5980).
  A `gh`-derived token that reaches a child never lands in the run's log or
  in the packaged deliverable unredacted, the same guarantee `boards`'
  credentials already carry.
- `trusty-audit add repo /path/to/checkout` and a `repos.txt` line naming an
  absolute path now audit a repository that is already on disk, which is the
  only way in when the org it came from is no longer reachable from this
  machine's GitHub credential. An ABSOLUTE path is a checkout and anything else
  is a GitHub `owner/repo` — a syntactic rule, so the same `repos.txt` means the
  same thing on every machine. Acquisition is `git clone <path>`, which hardlinks
  the object store and reads the source only: **the source checkout is never
  modified** — not a fetch, not a checkout, not a stash (a stash list is shared
  across every worktree of a repository, so that is real damage, not a
  theoretical one). The checkout lands under `repos/local/<basename>` and flows
  through the existing sweep to a report and an `extract/<repo>.db`
  indistinguishable from a remotely-cloned one, carrying committed history only.
  A path that does not exist, is not a directory, is not readable, is not a git
  repository, is a subdirectory of one, is a SHALLOW clone, or has no commits is
  refused at registration naming which condition failed — a `--depth=1` source
  would report one commit by one author over a zero-length period (#5916), so it
  is refused rather than audited thinly. A bare mirror is accepted: it carries
  the whole history. Two paths whose basenames match would share one checkout
  directory and are refused at both gates, naming both paths (#6001).

### Fixed

- Installing refuses a `tools/` area that is a symlink or a file rather than a real directory. A pre-planted symlink would send the recipient's binaries outside the working directory and survive the `rm -rf` the README documents as a complete uninstall; the hazard was inert while nothing installed there ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `trusty-audit clone` now records what it acquired in `state/selected-repos.toml`, so `trusty-audit clone acme/api acme/web && trusty-audit run` audits both. Before this, no code anywhere wrote that file — [#5555](https://github.com/bobmatnyc/trusty-tools/issues/5555) defined it and named repository selection and cloning as its producers, and neither implemented it — so the sweep refused with "nothing to audit" over a working directory full of checkouts ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- Only usable checkouts are selected. A repository that failed to clone is already a stated gap, and selecting it as well would fail it a second time for one cause, as a missing checkout in a sweep that had no way to know it was never acquired ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- An acquisition that produced nothing usable leaves an earlier selection alone, so a second `clone` that fails outright does not cost the operator the set that did land ([#5556](https://github.com/bobmatnyc/trusty-tools/issues/5556))
- The `tga audit` child now carries `TRUSTY_REVIEW_PROVIDER=openrouter` and the
  reviewer, verifier and summarizer model ids alongside the engagement's
  `OPENROUTER_API_KEY`, so review inference actually reaches OpenRouter. Naming
  the key was never enough: `trusty-review` resolves `Provider::Bedrock` as the
  last precedence level for all three roles, so the key sat unread while the
  reviewer either failed on missing AWS credentials or silently billed Bedrock
  ([#5671](https://github.com/bobmatnyc/trusty-tools/issues/5671)).
- The provider and the three model ids are resolved as one selection from a
  single layer — the operator's environment, else the engagement config, else
  the built-in slugs. A layer that names some of the four but not all four is
  refused before any child is spawned, naming what it set and what it left
  unset. Resolving them independently would pair an operator's
  `TRUSTY_REVIEW_PROVIDER=bedrock` with this crate's OpenRouter slugs, which is
  the same HTTP 400 reached from the other direction; nothing downstream catches
  that pairing ([#5679](https://github.com/bobmatnyc/trusty-tools/issues/5679)).
- An optional `[models]` table in `engagement.toml` overrides the built-in
  slugs, so a model rename is a config edit rather than a release. It rejects
  unknown keys, so `reviewr` is a parse error instead of a silent fallback to
  the default.
- A relative `--work-dir` (or `TRUSTY_AUDIT_WORKDIR`) no longer breaks
  `trusty-audit run`. `WorkDir::resolve` anchors a relative root to the caller's
  working directory, so the pinned `tga` binary, the generated tga config, the
  output directory and the extract database are all named absolutely to the
  child process — which runs with the work-dir root as its own cwd and
  previously failed to start with `os error 2`. (#5672)
- Two `trusty-audit add` runs against one working directory no longer discard each other's target. Registering and removing both load, mutate and save `state/audit-targets.toml`, and nothing made that sequence indivisible — the later save dropped the earlier one's entry while both reported success. Both now run under `trusty_common::file_lock::with_exclusive_lock`, the workspace's one implementation of that critical section ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- A `boards.jira.url` carrying `user:password@` userinfo no longer reaches a `Debug` render of the engagement config. The field is a plain string an operator may paste credentials into, and the derived `Debug` printed it verbatim; the userinfo is now stripped, leaving the site identifiable. The value itself is unchanged, so the request still carries it ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- A registered repository that failed to clone no longer vanishes from a
  one-shot `trusty-audit audit` run. Its failure was recorded only on the clone
  report, and because an unusable checkout is never selected, the sweep could
  not see it either — so the command exited 0 and handed back a package whose
  README said every repository was covered. The chain now folds the clone
  report's gaps into its own, which makes the exit status non-zero and puts the
  repository's name in the package's own `README.md` and `package.toml`.
- `trusty-audit distribute` names the member that actually failed. A write that failed on `README.md`, `audit.sh` or `engagement.toml` was reported as "taudit failed" — pointing the operator at the one member that had already succeeded ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- `Session::distribute` reads the packaging credential through an injected lookup rather than the live process environment, so `cargo test -p trusty-audit` can no longer write a developer's exported `OPENROUTER_API_KEY` into a zip on disk, and both credential sources are now asserted end to end ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- The return package could ship a repository's rendered report with its `extract/<stem>.db` collection database silently missing — `collect_extract` returned `Ok(())` whenever the database was absent, whether `extract/` itself did not exist or simply named nothing for that repository. Assembly now refuses with a named `MissingExtractDatabase` error instead, so the deliverable is always the database and the report together, never one without the other going unnoticed ([#5862](https://github.com/bobmatnyc/trusty-tools/issues/5862))
- The atomic state-file writer opens its temporary with `O_EXCL`, so it refuses whatever is already at that path instead of opening through it. The temporary's name is guessable — it is the pid plus a fixed-seed hash of a thread id — and a local attacker who pre-planted a symlink at it had the write follow the link: `trusty-audit`'s first-run prompt wrote the plaintext OpenRouter key into the attacker's file and left `engagement.toml` a symlink pointing there. Mode 0600 did not prevent it, because that mode constrains a file at creation and an open that follows a symlink creates nothing. A temporary left behind by a writer that crashed is still recovered, by unlinking and re-creating once ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `OPENROUTER_API_KEY` now wins over the key in `engagement.toml`. `trusty-audit run` used to hand the config's key to the `tga audit` child unconditionally, so an exported variable was silently ignored ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- A blank `openrouter_key` is refused before the sweep starts rather than an hour into it. A present-but-empty key was not a refusal anywhere downstream: `inference_env` read it as "select nothing" and returned no variables, so the `tga audit` child ran without its `TRUSTY_REVIEW_*` selection and `trusty-review` fell back to a provider nobody chose — surfacing at the report stage as a missing-AWS-credentials failure, or not at all ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- `trusty-audit package` scans the outbound deliverable for the key the sweep actually used. With a key supplied through the environment, the scan checked the config's key instead and would not have caught the real one ([#5868](https://github.com/bobmatnyc/trusty-tools/issues/5868))
- A credential echoed by the `tga audit` child — or by the `trusty-review` it
  spawns — no longer reaches the per-repository log file. Both piped streams are
  filtered on the way to disk through `trusty_common::credentials::scrub_secrets`,
  the workspace's one redactor, and the relay path decodes the scrubbed bytes so
  a key quoted inside a stage detail never reaches the operator's terminal either
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The needle set is every credential the process can resolve
  (`resolved_secret_values`), not only the engagement's OpenRouter key: a `gh`
  token embedded in a git remote URL is a different credential from a different
  source, and the narrow set would miss it. It removes only values this process
  already holds, so the log is lower-risk rather than proven clean
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The output pump no longer buffers a line without bound. A child printing one
  endless line with no newline is flushed in bounded pieces
  ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The filter sees a credential in one contiguous search space, so neither of the
  two boundaries the pump used to invent can split one into unmatched halves. It
  scrubs its whole buffer before cutting a piece off to write, rather than
  scrubbing only the piece — a key lying across that cut used to reach the log in
  two verbatim halves. It also searches a mixed-encoding segment over its
  valid-UTF-8 runs concatenated, rather than one run at a time — a key is valid
  UTF-8 and so cannot contain an invalid byte, but a child can inject one into
  the middle of it, and the two flanking fragments used to reach the log in the
  clear. What the pump still holds back at a flush is only a credential that has
  not finished arriving, counted in text bytes so injected garbage rides along
  with it ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- The size cap on that hold-back no longer writes out the credential it exists
  to keep. The hold-back is measured in text bytes and was capped in raw bytes,
  so around 4KB of non-UTF-8 output near a flush boundary — a binary diff or a
  corrupted pack object from a `git` child — pushed the cap past the text it was
  holding and wrote a partly-arrived key to the log in the clear; the rest of the
  key then arrived without its start and matched nothing, so both halves landed.
  The cap is now honoured by writing the invalid padding out of the hold instead,
  which leaks nothing, and the pump's buffer bound is unchanged at one segment
  plus the hold-back ([#5869](https://github.com/bobmatnyc/trusty-tools/issues/5869))
- `trusty-audit repos` no longer reads as a failed registration. It lists the companion `manifest.toml`, which `tga audit` writes once a sweep completes, so it is empty however many targets are registered — and its old empty-list message ("No repositories configured yet — run the guided flow to pick them") sent an operator who had just registered several repositories back to register them again. It now names `trusty-audit targets`, which answers the question that was actually asked.
- The README's verb table gains `add`, `targets` and `remove`, and states what separates `targets` from `repos`.
- The confirmed launch now runs the one-shot chain instead of the bare sweep. The
  sweep reads `state/selected-repos.toml`, which only `clone` writes and nothing
  in the guided launch wrote — so a fresh recipient died at
  `NoRepositoriesSelected` right after being told "Everything is in place", and a
  recipient carrying a selection from an earlier `taudit clone` had those OLD
  repositories audited, reported as audited, and exited 0.
- `trusty-audit guided` prints the status card and exits again. The interactive
  decision now reads the parsed CLI rather than the `Command` it maps to, which
  cannot tell the named verb from a bare launch — under a pty that turned the
  documented spelling into an unbounded hang.
- The sweep question discards terminal typeahead before it is asked, so an
  operator double-tapping Enter to finish adding targets no longer starts hours
  of unattended work with the second newline. Enter still starts the sweep.
- A board-only registry reports `SelectRepositories` again. Any registered target
  counted as a repository, so `taudit add board jira:ACME` skipped repository
  selection, triggered a real multi-tool download, and reported `ReadyForRun`.
- A bare launch prints its status card without asking for an inference
  credential. The key is resolved once the operator confirms the sweep, not
  before the card — a config with a blank key made the read-only launch exit
  non-zero after three mismatched retypes, never printing anything.
- A launch backgrounded with `&` prints the card instead of stopping silently.
  Opening `/dev/tty` for write succeeds from a background process group, so the
  first read raised SIGTTIN; the terminal probe now also checks the foreground
  group.
- The code-analysis leg reads the code again. `trusty-search` is default-deny,
  and nothing in the audit chain ever approved a clone, so `tga audit`'s
  `trusty-search index <checkout>` was refused on every repository of every
  run. It was silent: `tga audit` exits 0 whenever its sweep completed, so the
  run reported success and the report simply had nothing to say about the
  source. The sweep now runs `trusty-search index add <checkout>` before each
  child, and a refusal is a named per-repository failure rather than an empty
  section.
- Repositories are cloned with their whole history. `taudit clone` appended
  `--depth=1`, so tga read exactly one commit per repository and every
  deliverable said `commits=1`, `authors=1`, a period whose start equalled its
  end, and one author credited with the entire tree. Measured on
  `BurntSushi/xsv`: 1 commit by 1 author on 2025-04-24, against a real history
  of 407 commits by 30 authors from 2014-09-01. Every CSV was a header and one
  row.
- `trusty-audit-ui`'s guided-flow session builder (`guided.rs`) failed to compile against `WorkDir::resolve`'s new `home: Option<&Path>` parameter (#5929), which broke `origin/main` as a workspace. Both call sites — the production `session()` builder and its own test — now pass `dirs::home_dir()`, the same source `trusty-audit`'s CLI entry point uses, so the shell and the CLI keep resolving the same default work-dir root (#5935).
- A bare `trusty-audit` in a directory with no `engagement.toml` now sets the
  engagement up instead of registering targets against one that does not exist.
  It asks for the OpenRouter key first, writes `engagement.toml` at mode 0600
  with an exact version pinned per tool, preflights all four pinned tools,
  installs them, and asks what the audit covers LAST. Registration going first
  is what produced the reported launch — targets registered, `Tools: 0/4
  installed`, a command named to run next, and no key prompt at any point
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- A cold start whose `engagement.toml` cannot be written now stops and says so,
  naming the file and stating that the key was not saved. It used to be
  unreachable: nothing wrote the file, so the three gates that hit its absence
  each degraded quietly and none of them named it
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- The scrubber that keeps a credential out of a spawned child's log, and the
  guard that refuses to package a file carrying one, both now cover the
  `gh`-derived GitHub token — previously only `EngagementConfig`'s own
  secrets were checked, so a rejected token echoed back by a child could
  reach the log or the deliverable unredacted (#5980).
- Packaging now refuses when the active `gh` credential differs from the one
  the sweep collected under, instead of silently scanning outbound files with
  the wrong token. Packaging can run as a separate process from the sweep —
  possibly under a different `gh` account after the operator re-authenticated
  — and the previous outbound scan re-resolved the credential fresh each
  time, so a token a child echoed at sweep time could ship in the deliverable
  unredacted because the newly-resolved token never matched it. A truncated,
  non-reversible fingerprint of the sweep's credential is now recorded in the
  checkpoint and compared at packaging time; a checkpoint written before this
  fingerprint existed still packages, with the uncertainty stated rather than
  silently assumed safe (#5980).
- `taudit add board linear:<team-id>` now registers the team's short key, so the
  board collects the issues it was registered for. Validation accepted a team id
  and stored it verbatim; collection matches `linear.team_keys` against the text
  before the hyphen in `ENG-1234`, which a team id never occupies, so the board
  validated green and contributed nothing on every subsequent run. The same
  round trip also stores Linear's own casing, which that exact-match filter is
  equally sensitive to.
- Registration now refuses a team whose key the sweep could never match, rather
  than persisting it. `validate` documented that a registered Linear key is one
  `is_linear_team_key` accepts and nothing enforced it: the reply's `key` field
  is optional, so a reply that omitted it registered an empty key — an entry that
  validated green, could never collect, and could not be removed either, because
  `taudit remove linear:` is not a spelling the parser accepts.
- A Linear team key already on disk is uppercased on its way to the sweep rather
  than skipped. tga reads a stored key twice and disagreed with itself: its
  collector compares team keys ignoring case, so `linear:eng` did collect
  `ENG-1234`, while its classifier compares exactly, so the same issue classified
  as nothing. Uppercasing keeps the collection and adds the classification.
- A Linear board that no normalisation can save is now stated as a gap on
  `taudit run` as well as `taudit audit`, and the run exits 2 rather than 0. The
  sweep resolved the boards, discarded the gaps, and returned "every repository
  audited" with no `linear:` section and nothing said — the silent skip the gap
  line exists to replace.
- That gap line now describes the key shape it needs instead of asserting the
  stored key is a team id, and names both halves of the remedy: registering the
  team key leaves the old entry behind, and a registry still holding it keeps
  reporting the board as not audited.
- A `repos.txt` or `boards.txt` written by Notepad, Excel's "Save as .txt" or PowerShell's `Out-File` starts with a byte-order mark, which is not whitespace and so survived `trim` into the charset check — refusing line 1 and, under the all-or-nothing rule, every repository in the engagement. The mark now comes off once at the file level; one anywhere else is still a malformed entry ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- `https://github.com/orgs/<org>/repositories` — the natural paste for "audit every repository in this org" — was read as the target `orgs/<org>`, so the operator got a refusal naming a repository they never listed. A URL whose first path segment is one github.com reserves for itself is now refused with a line saying to list each repository instead. `/issues/12`, `/blob/main/…` and `/tree/…` URLs still parse ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- A launch with no terminal whose targets files registered nothing exited 0, so a wrapper script reading `$?` saw success over a 0-of-14 outcome. Any shortfall now exits 2 (`EXIT_INCOMPLETE`) and prints how many of the listed targets did not register ([#5990](https://github.com/bobmatnyc/trusty-tools/pull/5990))
- Two local checkouts whose basenames differ only by case — `/srv/a/Apex` and
  `/srv/b/apex` — are now refused as a `CollidingCheckouts` at both
  registration and `clone_all`, instead of silently landing in one checkout.
  On a case-insensitive, case-preserving filesystem (APFS's default, and the
  one this feature runs on) `repos/local/Apex` and `repos/local/apex` are ONE
  directory, but the derived name kept its case, so the second repository was
  reported as audited having actually read the first one's history (the
  #5896 wrong-corpus family). The collision comparison in both gates is now
  case-folded, unconditionally.
- `trusty-audit add repo`'s usability check for a local checkout now asks
  `--is-bare-repository`, `--is-shallow-repository`, and `--show-toplevel` in
  one `git rev-parse` invocation instead of two, tolerating the toplevel flag
  failing on its own for a bare repository.

### Changed

- `trusty-audit tools` now reports the verified version beside each binary, and marks a binary this client did not place as `UNVERIFIED` rather than showing it as installed — a version it did not verify is one it cannot vouch for ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `Session::execute` is `async`. Installing downloads, and blocking on a runtime inside a synchronous `execute` would work for the CLI and then panic inside the Tauri shell, which calls from an async context ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- An engagement config that omits the `trusty-search` pin no longer loads. Add the
  key to any existing config before the next run.
- The guided flow and `trusty-audit run` now install the pinned tools they are
  missing instead of reporting them and stopping. The guided flow installs once
  repositories are chosen — the point it used to print "install the tools" — and
  `run` installs before the sweep's preflight. Both call the same all-or-none
  `trusty-installer` entry point `trusty-audit install` does, so a set that
  cannot be fully resolved installs nothing and the command fails; the #5454
  guarantee is reached earlier, not relaxed
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- A binary the client did not place — the `UNVERIFIED` row in `trusty-audit
  tools` — is reinstalled rather than kept. Nothing may claim a version for it,
  and an unknown version is the #5454 version-skew input; `tools/` is the
  client's own area under the work-dir root, so replacing it costs nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- The temp-file-then-rename discipline the state files rely on moved to `workdir::write_atomically`, so `selected-repos.toml` and `audit-targets.toml` share one writer rather than restating it ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
- Each `tga audit` child's stdout and stderr are piped and teed rather than
  pointed straight at the log file. The per-repository log is unchanged in
  content; a repository whose child output could not be written to it is now
  recorded as a failure rather than left as a silent gap in the evidence
  ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
- A registered repository target now reaches the sweep. `state/audit-targets.toml`
  had no reader until now — `run` took its input from the selection file only
  `clone` wrote — so registering a repository did nothing until someone cloned it
  by hand under the same name. The one-shot `audit` command clones what is
  registered.
- A registered board is reported as a stated gap rather than silently skipped.
  Passing one to `tga audit` would mean writing the board credential into the
  generated tga config on disk, which this client will not do.
- The board credential reaches `tga` the way the OpenRouter key already does:
  the generated `state/tga-<stem>.yaml` carries `${TRUSTY_AUDIT_JIRA_TOKEN}` /
  `${TRUSTY_AUDIT_LINEAR_API_KEY}`, and the sweep puts the real value in the
  child's environment. No secret is written to a file. An unset variable is a
  `tga` config error naming the field, not a silent unauthenticated read.
- A board is still stated as a gap when the engagement config carries no
  credential for its provider, and when a second JIRA project is registered —
  `tga`'s `jira.project_key` holds one.
- The return package refuses a file carrying the JIRA token or the Linear API
  key, as it already did for the OpenRouter key. Those two secrets now travel to
  a `tga audit` child, and the files that child writes are the files the package
  sends off the recipient's network.
- The child-log scrubber strips the board credentials too. It built its needles
  from the registered providers plus the OpenRouter key, and no provider there
  is a board, so a `tga` child quoting a JIRA token in an auth error wrote it to
  `work/logs/<repo>.log` in the clear. The packaging guard and the scrubber now
  draw their secrets from one list on `EngagementConfig`.
- `taudit audit` reads the target registry once. It used to read it again when
  the sweep started, hours later on a real engagement, so a board removed
  between the two reads left the report claiming coverage while nothing
  collected it — a zero exit over an engagement that skipped a registered board.
- `cli::registration::is_interactive` takes `&Cli` rather than `&Command`, and
  `guided_at_the_terminal` returns `Launch` rather than an `Outcome` so the
  front end owns when the credential is resolved.
- `README.md` no longer offers `TRUSTY_AUDIT_NO_LAUNCH=1` as a way to make the
  binary non-interactive. It is an `install.sh` variable deciding whether the
  installer starts the binary; the binary never reads it.
- The default working directory is `~/.trusty-tools/trusty-audit/work`, and the
  packaged launcher no longer pins `--work-dir` beside itself. The tree used to
  land wherever the recipient unzipped the package, and `trusty-search` refuses
  to index a checkout under `/tmp`, `/var/folders`, `~/Downloads`, `~/Desktop`
  or `~/Documents` — which is where an emailed zip gets opened — so the
  placement cost the code analysis its data on an ordinary machine. `--work-dir`
  and `TRUSTY_AUDIT_WORKDIR` still override, and the launcher forwards them.

  Two costs, both stated in the package README: approving a clone writes a row
  to `trusty-search`'s own allowlist, outside the work root, so deleting the
  root is no longer a complete uninstall (`trusty-search index remove <path>`
  undoes it); and an existing operator's tree does not move itself — pass
  `--work-dir` at the old location, or move it.
- The built-in reviewer default is `anthropic/claude-opus-4.8`, was
  `anthropic/claude-sonnet-4.6`. This changes behavior for EXISTING configs, not
  only for newly written ones: an auditor-supplied `engagement.toml` with no
  `[models]` table resolves through this constant, so the judging call on the
  common handoff path moves to the Opus analysis tier. The verifier and
  summarizer stay on `anthropic/claude-haiku-4.5`, and a config that names its
  own `[models]` table is unaffected — it outranks the built-in
  ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
- `engagement.toml` now declares what an engagement audits, in a `[[targets]]` array, and `<work-dir>/state/audit-targets.toml` is a working copy rebuilt from it. An engagement is described by one portable file: hand someone the config and they have the key, the models and the scope ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- `taudit add repo` / `add board` / `remove` write the engagement config, under the same cross-process lock the registry took, and the config is published at mode 0600 before the working copy is mirrored — so a config write that fails leaves both files untouched ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- An engagement whose config declares no targets adopts whatever the working copy holds, so an upgrade keeps every target that was registered before this change; the first `add` or `remove` persists the adopted set into the config. `targets = []` is a declaration of zero and does not adopt ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))
- `taudit add` and `taudit remove` now refuse in a directory with no `engagement.toml`, naming the file and the command that creates one, rather than writing a registry nothing treats as authoritative ([#5979](https://github.com/bobmatnyc/trusty-tools/issues/5979))

### Removed

- `taudit clone --full` and `CloneOptions::shallow`. A full clone is now the
  only mode, so the flag had nothing left to select and the field was one
  assignment away from re-emptying the deliverable. Disk stays bounded by
  `--budget-gb`, which is unchanged: the same repository is 628 KiB shallow and
  1.2 MiB full, against a 20 GiB default.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

