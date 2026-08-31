# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [1.5.12] — 2026-08-31

### Added

- **The launch path warns when a checkout carries another harness's (claude-mpm) hook wiring**, reusing `tm doctor`'s own `hooks_foreign_conflict` detector so the finding lands when the session starts rather than only when someone runs the diagnostic. Report-only — tm never removes another harness's hooks
- The `TM <ver>` text in `tm statusline` is now a clickable link to the
  trusty-console. Cmd-click (macOS) or Ctrl-click opens the dashboard on the
  port the console is actually selected for — read from its discovery file,
  falling back to the default `7788` when no console is configured. The link is
  an OSC 8 escape sequence, the form Claude Code's status line documents for
  clickable text, so the port lives in the escape and no rendered column gets
  wider or gains a port that #6304 removed. A terminal without hyperlink
  support shows the same `TM <ver> ●` text it showed before.
- `discovery::console_addr` and `discovery::console_base_url` are the single
  entry point for the console's selected address. The gateway resolver's
  inline discovery-file read moved into them, so the statusline and
  `resolve_daemon_url_via_gateway` now resolve the console the same way instead
  of keeping two copies of the fallback rule. Neither probes the network.
- An agent worktree is now a registered, reapable child of the delegation that dispatched it (#4311). The agent reports its own worktree path on its first tool call — every subagent hook payload already carries `agent_id` beside the `cwd` it is standing in — and the daemon records it on the delegation. trusty-mpm still creates no worktrees; what it gains is an owner for one the harness made, which is the condition ADR-0020's fail-closed rule needs before anything may remove it.
- `SubagentStop` now reaps that worktree, behind six refusal gates: the tree survives if the harness already reclaimed it, if it is not a `.claude/worktrees/<name>` leaf, if a live session or an unfinished sibling delegation still holds it, if no git registry claims it, if it holds unsaved work, or if `git worktree remove --force` does not succeed. The unsaved-work gate is #4091's own implementation, not a second copy, and there is no path through which this reap can reach the force-discard policy. The branch is never deleted, and there is no `remove_dir_all` fallback. A `Stale` delegation is not reaped — that status means tracking lost the agent, not that it exited.
- Agent worktrees are now attributed on disk, so ownership survives a daemon restart (#4311, DOC-66 §1.3). The `.trusty-mpm-worktree` sentinel payload widens from `{owner_session_id, created_at}` to also carry an `agent` block — the subagent's `agent_id`, its delegation id, and the dispatching session id. It is written when the agent's first tool call reports its working tree, which is the only moment the harness-chosen path is knowable. `DaemonState::delegations` is a `DashMap` built empty at every boot with no load path, so before this a restart dropped every registration permanently; `SubagentStop` now falls back to reading the sentinels under `.claude/worktrees/` and matching the exact `agent_id`, which is the from-disk rebuild ADR-0023 point 4 requires.
- Four claims stand in front of that write, because the `cwd` an agent reports is `std::env::current_dir()` of its own `tm hook` process and moves wherever the agent `cd`s. The path must be a `.claude/worktrees/<name>` leaf (the same predicate the reap uses, reused rather than restated); the store it sits in must belong to the dispatching session's own checkout; its existing sentinel must be absent or already this agent's; and no other running delegation may already register it. The last claim is what covers a peer worktree carrying no sentinel — which is every directory in the store until they acquire one. An agent that steps into its main checkout, a scratch directory, another project's store, or a peer's worktree now writes nothing there and registers nothing.
- The reap reads the target's sentinel and refuses unless it names the exiting agent — once before the expensive gates, and again immediately before the removal, following the #4118 precedent that the authoritative verdict is the one adjacent to the deletion. The path it is handed comes from the delegation registry or from the disk scan; neither is a claim made by the directory itself, and the sentinel sitting in the target is the only one that cannot have been redirected on the way there.
- A live-process gate now guards the removal. Every other gate reads a registry — what trusty-mpm recorded, or what git recorded — and neither sees a process nobody registered; on 2026-08-15 a `trusty-memory serve --foreground` started by hand inside an agent worktree ran for a day in exactly that blind spot. The new gate runs one `lsof -d cwd` and refuses when any process's working directory is inside the tree. It fails toward IN USE (ADR-0045), and proves it can see at all by requiring the listing to report the probing process itself — without root `lsof` returns only the caller's own processes and exits 0, so a partial listing would otherwise read as a clean negative. It catches long-running commands an agent started from inside its tree; it does not reliably catch the agent process itself, whose cwd is normally a project root, and which the registry covers instead.
- **`tm reinstall --binary` refuses a source behind `origin/main`.** `tm` is one
  global binary shared by every managed session on the machine, and the `--path`
  reinstall route rebuilds it from whatever source directory cargo's ledger
  recorded — so installing from a worktree that predates a fix regressed that
  fix for every session at once, silently, because the binary's own version
  number does not move when its source is merely stale. The command now fetches
  and compares `HEAD` against `origin/main`, refusing when it is behind and
  naming `TRUSTY_MPM_ALLOW_STALE_INSTALL=1` as the deliberate override. It
  refuses on positive evidence only: a source that is not a git repository, has
  no `origin/main`, or has no usable `git` warns and installs
  ([#4462](https://github.com/bobmatnyc/trusty-tools/issues/4462))
- `tm memory import --refresh` (#5044) replaces a drifted drawer with the
  file's current text — `memory_forget` then `memory_remember`, since
  trusty-memory has no update-in-place tool — instead of reporting the drift
  and leaving the stale copy in the palace. The plain run is unchanged and
  still refuses to touch a drifted drawer; its skip reason now names
  `--refresh`. Two new report statuses, `refreshed` and `would-refresh`, and a
  `refreshed` count in the JSON report and the summary line.
- Under `--refresh`, every file whose drawer the run names is put through
  `palace_verify_embedded` (#6328) and fails unless that drawer is retrievable
  right now. The `#4834` flow deletes source files on a clean exit, so a drawer
  that is stored but absent from vector search — or a gate that could not run
  at all — blocks the run rather than passing it. `--dry-run --refresh` reports
  the same verdicts without writing.
- A refresh that removes the stale drawer and then fails to write its
  replacement reports `DATA LOSS`, names the removed drawer, and says not to
  delete the source. A refresh whose `memory_forget` fails reports that nothing
  changed. The two arms never read alike.
- `git pull`, `git merge` and `git rebase` are now denied in a project's main checkout when the daemon reports another agent writing in that same directory (ADR-0048 decision 10). All three move the shared git HEAD under whoever else is standing on it, and git reports no error when they do. The rule binds the PM and every agent it dispatches. A pull in a worktree, a pull in a checkout nobody else is writing in, `git fetch`, and the `--abort`/`--continue` family stay allowed, and the denial names `git fetch` and a worktree as the two remedies.
- A worktree the guard grants a dispatched writer is now reported to the daemon, so the dispatch's delegation record carries the isolation it was given. Without it the daemon's own tracker recorded the original unisolated payload, every granted writer stayed named as writing in the shared checkout, and the new HEAD-move rule then denied `git pull` there for six hours on an agent that had already been moved out — blocking the release flow's fast-forward of the main checkout.
- A dispatch made from a main checkout is denied, rather than granted a worktree, when the daemon reports another agent already writing in that checkout. The grant used to return before the concurrent-dispatch check ran at all, so a second unisolated writer that would previously have been denied was admitted whenever the harness did not apply the rewritten dispatch input.
- A dispatched agent that may write files is now granted its own worktree when the session is standing in a project's main checkout: `tm hook --pm-guard` rewrites the dispatch to declare `isolation: "worktree"`, so the harness provisions and reclaims the tree (ADR-0048). An agent this binary cannot classify is treated as a writer there rather than trusted.
- The ADR-0044 write boundary is enforced. In a main checkout, an edit tool targeting a source file and `git commit` are denied for the PM and for every agent it dispatches; documents, configuration, and everything in a worktree stay writable. Each denial names the remedy.
- The interactive picker can ask for an isolated session: `n <name> --worktree` (the flag also leads, and works alone) sends the same `worktree` request `tm launch --worktree` sends, so a long refactor or a risky rebase no longer needs a different command. Without the flag nothing changes — the launch still lands in the project's main checkout.
- A launch that joins a checkout other live sessions are already standing in prints one line naming them, the ordinal, and the `--worktree` alternative. It states where the session lands; it does not warn, prompt, or refuse, and the first session in a project sees nothing.
- A project can now decline the dispatched-agent worktree grant. Set `agent_worktree = false` in the project's committed `.trusty-mpm.toml` and every agent dispatched there stays in the main checkout: no worktree is created, and none needs reclaiming (#5814). The dispatch's prompt is annotated with the workflow that replaces isolation — edit in place, commit on the branch the checkout already has, push. Isolation stays the default everywhere else; an absent, malformed, or unreadable config keeps ADR-0048's grant exactly as it was.
- `agent_worktree` is a separate key from `worktree`, which decides where a managed SESSION runs. Setting one does not move the other. The opt-out exempts the dispatch from the worktree grant only: the ADR-0044 main-checkout write boundary is unchanged, and a second concurrent writer in the same checkout is still refused.
- `/tm-init` now adds a **Build Performance** pointer to a Rust project's
  scaffolded or refreshed `CLAUDE.md` (keyed off the same `Cargo.toml` marker
  `rust-engineer` deploys on), so build-performance discipline is part of
  standard project setup instead of something reached only after a build
  already feels slow. The pointer references the bundled
  `rust-build-performance` skill and directs a new contributor to
  `cargo build --timings` for a measured baseline. It deliberately does not
  assert that `sccache` (or any shared compilation cache) makes builds
  faster: for a workspace's own path/member crates — the common
  multi-worktree cold-build scenario — sccache under its default config gets
  zero benefit, since cargo's dev profile builds those crates incrementally
  and sccache cannot cache incremental output. `CARGO_INCREMENTAL=0` is
  named as the only lever for a cross-worktree hit on path crates, without
  recommending it — that tradeoff is unmeasured. Whether sccache pays off on
  a workspace's external, non-path dependencies stays a separate, unanswered
  question the pointer directs the operator to measure (`sccache
  --show-stats`) rather than assume.
- `tm wait` — a condition-based wait an agent can run in-turn. Foreground `sleep` is rejected by the guards and the harness auto-backgrounds a foreground call near ~120s, so an agent facing a cold build measured at 1h04m ended its turn and lost the work. `tm wait` polls a condition instead: `--for run` (a PID or a file-backed job handle), `--for file` (a sentinel's existence, or a literal it must contain), and `--for check` (a PR's checks settling, through one-shot `gh pr view` reads — never `--watch`, and never trusting `bucket` or an empty rollup). One invocation returns before the harness ceiling with `status=pending` and exit 75, carrying the remaining budget and the exact command to re-issue; `status=met` exits 0 and `status=timeout` exits 1. The hard `--timeout` spans re-runs, so re-issuing the command does not reset the agent's own deadline ([#5843](https://github.com/bobmatnyc/trusty-tools/issues/5843))
- `tm projects register --gh-config-dir <dir>`, beside the existing `--gh-account`, so one command sets both keys per-project `gh` selection requires — `github.config_dir` and `github.account`. #5851
- `tm doctor --fix` now redeploys drifted or missing output styles under `~/.claude/output-styles/`, honouring the existing dry-run-by-default / `--yes` convention. A file that cannot be read is refused rather than overwritten ([#5866](https://github.com/bobmatnyc/trusty-tools/issues/5866))
- `tm ls --no-prune` and `tm sessions ls --no-prune` make a listing a pure
  read: no dead-record decommission, no confirmation-marker write, no prune
  notice (refs [#5950](https://github.com/bobmatnyc/trusty-tools/issues/5950)).
  Auto-pruning every listing stays the default (#4702) and is now stated in
  both commands' `--help` rather than only in a doc comment. The prune's
  operator lines were already on stderr, so `--json` stdout is parseable JSON
  either way.
- The PM instruction package's workflow section now states merge-queue ownership: one
  session owns a repository's merge queue at a time, a merge authorization is scoped to
  the PRs presented when it was given, an outstanding review verdict (a `code-critic`
  BLOCK, a requested-changes review, a hold label) blocks a merge that green required
  contexts would otherwise permit, and a hold is marked in GitHub state rather than
  announced by message. Procedure in the `tm-workflow` skill; pointer from
  `tm-delegation-patterns`' Cross-Workstream Coordination.
- `tm session prune --state unresolvable` selects managed records that name no
  resolvable workspace — `cwd` is the `/unknown` sentinel and there is no
  `workspace_path`
  (refs [#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118)).
  These are the adoption ghosts #6126 stopped minting; nothing could select the
  ones already in the store. On the reporting host 23 of them sat `Active`:
  every existing `--state` value selected 0, `prune-idle` skipped all 23 on an
  `errored` verdict, and the only command that reached them — `--state all
  --include-active` — also selected 32 healthy working sessions including the
  operator's own. The new filter is the one selector whose choice ignores tmux
  liveness, because a live pane is what made the class unreachable; it needs no
  `--include-active` and is not widened by it. Such a record has no
  `workspace_path`, so this path can delete no file: it reclaims the leaked pane
  and the store row. Two records sharing one pane (the #6117 race) are both
  tombstoned in one pass.
- `tm session decommission-ephemeral --dry-run` previews the sweep instead of
  performing it. Preview and real sweep are one call differing in one boolean, so
  they cannot select different sets. The daemon echoes `dry_run` and the CLI
  refuses to report a preview the daemon did not confirm — a daemon predating
  this change drops the unknown query param and sweeps for real while still
  returning 200.
- `tm` and `tm launch` in a directory that is not a git repository now run
  `git init` there and carry on, instead of stopping at "not in a git project"
  or "no git origin remote found"
  ([#6274](https://github.com/bobmatnyc/trusty-tools/issues/6274)). The
  repository lands in the invocation directory, a one-line
  `tm: initialized git in <path>` notice says so, and everything after that runs
  exactly as it does for a git repository with no origin remote. Three
  directories are left alone: the home directory and the filesystem root refuse
  with a stated reason, and a directory that already has a repository — its own,
  an ancestor's work tree, or a bare repo — is untouched. A missing `git`
  executable is a prerequisite error naming git; nothing is written in that case.
- `TRUSTY_SEARCH_SOCKET` pins the trusty-search daemon's socket path for a probe,
  replacing `TRUSTY_SEARCH_ADDR`. It exists for a test rig or a CI job pointing
  at a daemon it started itself; `TRUSTY_DATA_DIR_OVERRIDE` is process-global and
  would redirect every other trusty-* client in the same process.
- The daemon binds a hardened Unix socket at `<data dir>/trusty-mpm.sock`
  alongside `127.0.0.1:7880`, through the same
  `trusty_common::uds::bind_singleton_hardened` entry point trusty-memory,
  trusty-review, and trusty-analyze use — a `0600` socket in a `0700` directory,
  with a peer-uid check on every accepted connection. The listener serves an
  `RpcRouter` with no registered methods, so every request answers
  `method_not_found` until slice 2 moves the first route across; both listeners
  drain on the same SIGTERM/SIGINT and the socket file is unlinked before the
  listener is dropped. A socket that cannot be bound fails startup with an error
  naming the path rather than degrading to an HTTP-only daemon. No HTTP route
  moved and nothing was removed (slice 1 of
  [#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288), ADR-0032).
- `daemon::socket` — `socket_path`, `bind`, and `serve_until_shutdown`, the
  listener's bind and serve halves, split so a test can drive the real path with
  its own shutdown future.
- The daemon serves its health, doctor, error-list, bug-report, breaker, optimizer, overseer, LLM-chat, tmux and claude-config routes as JSON-RPC methods on the Unix socket, under `mpm.<family>.<verb>` names. HTTP still serves all twenty routes unchanged; each route now has one shared body both transports call. (#6288)
- Sixteen more JSON-RPC methods on the daemon's Unix socket: the legacy session
  registry (`mpm.sessions.list` / `.register` / `.connect` / `.discover` /
  `.reap` / `.get` / `.delete` / `.pause` / `.resume` / `.command` / `.output` /
  `.pane` / `.set_pid` / `.events_poll`), the hook relay (`mpm.hooks.ingest`),
  and the polled event feed (`mpm.events.poll`). The HTTP routes they mirror are
  unchanged and still served; the socket is an additional way to reach the same
  body. The SSE streams `/events` and `/sessions/{id}/events` stay HTTP-only for
  now ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- The daemon's managed-session lifecycle, SESSCTL control-plane, and L2 proxy routes are now also served as JSON-RPC methods on the Unix socket (`mpm.managed.*`, `mpm.control.*`, `mpm.proxy.*`). HTTP is unchanged and every route still answers on it; each method and its HTTP route share one implementation, so the two transports cannot drift.
- The #6197 input validation and caller check moved into that shared implementation, so neither transport can reach a control-plane spawn without them. The socket's own peer-uid check is stricter than the loopback guard it stands in for.
- `mpm.control.connect` answers as a frame stream, carrying the same session events the SSE route's `data:` lines carry.
- The daemon serves its registry-B project, deliverable/milestone, L3 manager, peer-bus, pairing and delegation-query routes as JSON-RPC methods on the Unix socket, alongside the unchanged HTTP surface (33 methods, `mpm.projects.*` through `mpm.delegation.*`).
- The peer bus's SSE `subscribe` leg is deliberately not among them — it needs the streaming seam a later slice adds, and its HTTP handler is untouched.
- `session_delete_records` MCP tool: record-only bulk deletion over an explicit
  session-id list. Each id routes to whichever registry owns it — the managed
  store's `delete_record`, or the legacy in-memory registry — and the call never
  removes a worktree, a workspace directory, or any other filesystem state, and
  never kills a tmux host. Fail-closed: a session that is still running is
  refused in both registries (there is no `force`), a liveness probe that cannot
  reach a verdict refuses too, and a malformed, unknown, or refused id is
  reported as one failed row rather than counted as a deletion. A deleted legacy
  entry that shares its tmux name with a live managed record reports that record
  as `managed_sibling` and leaves it untouched (#6431).

### Fixed

- **A launch with no worktree request now runs in the managed checkout (`<workspace-root>/<owner>/<repo>`), not in whatever directory `tm` was typed in** — launching from an unmanaged clone switches to the managed checkout and provisions it when absent. The unmanaged tree is never written to, and a managed checkout that cannot be provisioned fails the launch instead of falling back to it (ADR-0037 terminology clarification)
- **An explicit `--worktree` launch whose worktree cannot be established now fails** — it previously logged a warning and spawned a session with no worktree at all, reporting success for a placement nobody asked for
- `tm hook --pm-guard` now classifies process-substitution bodies. `<(…)` and
  `>(…)` join `$(…)` and backticks as one class of substitution the guard
  decomposes, so `diff <(sed -i s/a/b/ f) x` — which bash executes, and which
  the guard allowed — is refused, and `>(…)` is denied by its body rather than
  incidentally by the `>` redirection scan. An unbalanced opener fails closed,
  matching the `$(`/backtick shape; a quoted `<(…)` in a commit message stays
  literal text. (Refs #2745)
- `tm doctor` gained a `base_clone` check. A linked worktree keeps every source
  file when the clone behind it loses its git internals, and then every git
  command there fails with `fatal: not a git repository` — the 2026-07-21 state
  that went unnoticed for over half an hour across 70 worktrees while both
  worktree probes stayed green. The check reads each live session workspace's
  `.git` pointer and Fails naming the base path, what is missing, and how many
  live worktrees hang off it. Detection only: it never repairs, moves, or
  deletes, and its remediation text keeps the existing quarantine-never-delete
  discipline. (Refs #3605)
- Deploy diagnostics now reach the operator on every in-process CLI path. Bare
  `tm` (including the in-place relaunch), `tm doctor --fix-skills`, and
  `tm catalog apply` each run a deploy that legitimately declines files — a
  checksum-frozen skill, an unreadable ledger, a raced merge — and each decline
  was written to a `tracing` subscriber that was never registered, so the file
  stayed stale forever with no signal. All three now install the same
  stderr-only subscriber `tm sessions instructions` uses, at the `warn` default
  so a clean run stays quiet. Registration is `try_init`, so a second
  installation returns an error instead of aborting the process. (Refs #4878)
- Peer-bus registration no longer overwrites a live instance on an instance-id
  suffix collision. `DashMap::insert` replaced the existing entry, after which
  every lookup of the first instance resolved to the second — instance-addressed
  delivery reaching an instance the sender did not name. Registration now claims
  the key through `entry`, re-minting up to eight times and logging each
  collision, and returns the new `BusError::InstanceIdCollision` (HTTP 409)
  rather than displacing a registered instance. (Refs #4276)
- `~/.trusty-mpm/daemon.lock` is no longer deleted by a process that does not
  own it (closes [#1731](https://github.com/bobmatnyc/trusty-tools/issues/1731)).
  The daemon has written the file at startup since #1731 first closed, but the
  `daemon::lock` unit test called `write_lock` then `remove_lock` against the
  real path to prove they do not panic — so every `cargo test -p trusty-mpm` run
  overwrote a live daemon's record with the test process's PID and then removed
  it, leaving a running daemon with no lock file. Removal is now
  ownership-checked in both the daemon's shutdown handler and `tm stop`, so an
  outgoing daemon cannot delete a successor's record either, and that test is
  hermetic.
- `PATCH /api/v1/projects/{name}` no longer stores a `gh_user` that `gh` has
  never heard of
  (closes [#2121](https://github.com/bobmatnyc/trusty-tools/issues/2121)).
  The endpoint accepted and persisted whatever string it was handed, so a
  typo'd or not-yet-authenticated login returned 200 and surfaced much later as
  a delegated-agent failure. Setting the field now runs one bounded
  `gh auth status` and rejects the whole request with 400 when the login is not
  one of the logged-in accounts, naming the field, the rejected login, and the
  accounts that would be accepted; an accepted login is stored in `gh`'s own
  spelling, so the byte-for-byte comparison `ensure_gh_account_in_dir` makes
  later cannot disagree with it. A probe that cannot answer — no `gh` on PATH,
  or a timeout — is 503, never a silent accept and never a 400. Clearing with
  `null` and any request that does not mention `gh_user` run no probe at all.
- **Two sessions can no longer share one tmux pane.** The managed create path
  ran `tmux new-session -A`, and `-A` attaches to an existing session of that
  name instead of failing — so two creators that computed the same name both
  returned success, and two session records were persisted driving one
  terminal, with no error at any step. The create now runs without `-A`: a
  creator that loses the name gets a refusal, folds that name into its taken
  set, and retries under a fresh one, up to four attempts before failing with a
  name collision. The orphan-reaping guard is armed only after a create that
  actually created something, so a loser can never reap the winner's session
  ([#3707](https://github.com/bobmatnyc/trusty-tools/issues/3707))
- A `SubagentStop` that reached the daemon before the `PostToolUse` teaching its
  `agent_id` no longer strands its delegation `Running` for six hours. The stop
  is held in a bounded, TTL'd ledger and applied the moment the id is taught, so
  the two arrival orders converge on one record. Resolution still matches only
  the exact `agent_id` the stop quoted — nothing guesses at a "most recent"
  delegation, and an entry the ledger drops leaves the delegation to the
  staleness sweep exactly as before (#4142).
- **A session provisioned from a repo URL no longer leaks its git branch.** `provisioner::workspace::provision_in` named the worktree's branch bare (`<session-id>`) while `session_manager::decommission::remove_session_worktree` force-deletes `session/<session-id>`, so the delete missed and one branch accumulated in the base clone per session. The provisioner now names the branch through `core::worktree_naming::worktree_branch_for`, the single convention both sides read ([#4165](https://github.com/bobmatnyc/trusty-tools/issues/4165))
- **The worktree reclaim guard no longer reads CLEAN on a candidate holding a nested BARE repository.** It decided "is this a repository root" by matching an entry named literally `.git`, which a bare repository does not have — so a worktree whose gitignored `scratch/salvage.git` held the only copy of a commit reported `dirty_files = 0` and was deleted. A root is now recognised by that entry OR by the `HEAD` + `objects/` + `refs/` triple, and the hit is routed through the same object-store discriminator as any other nested repository, so a bare repo whose store survives the candidate is still not counted as dirt. Stopping at the root also stops the walk spending its 50k-entry scan budget inside `objects/`. A short list of gitignored names that are unrecoverable if deleted — `.env*`, `*.bak`, `*.orig` — now counts as unsaved work as well; anything under a disposable build directory (`target/`, `node_modules/`, …) stays excused, so a candidate whose only ignored content is build output is still reclaimable. Measured read-only across 27 live worktree leaves: 1 becomes newly dirty, naming a real pre-edit `settings.json.bak` ([#4166](https://github.com/bobmatnyc/trusty-tools/issues/4166))
- **A tracked file deleted from `.trusty-mpm/` is counted again.** Pass 1 skips every `.trusty-mpm/`-scoped status line on the promise that pass 2 owns them, and pass 2 returned 0 without running git whenever the directory was absent — which is exactly what deleting the last file under it produces, so the deletion was counted by neither pass ([#4166](https://github.com/bobmatnyc/trusty-tools/issues/4166))
- The peer bus no longer records an envelope as `delivered` when a lagging
  subscriber never saw it
  ([#4271](https://github.com/bobmatnyc/trusty-tools/issues/4271)).
  `broadcast::Sender::send` answers `Ok` for a receiver that is attached but
  behind, so publishing into a full 64-deep instance channel overwrote an unread
  envelope while the DOC-60 §9 durable log wrote `delivery_state: "delivered"`
  for the one that displaced it — the sender saw `202 Accepted` and the
  recipient was never told. `PeerBus::publish` now checks the channel before
  sending and refuses the new message with `SubscriberLagged` (`503`, or
  `CODE_UNAVAILABLE` over the socket), recording it `dropped`. What the log
  calls delivered is now exactly what the subscriber receives.

  Operationally this trades a silent loss for a visible refusal, and the
  refusal is per-instance: a subscriber that stops draining makes every publish
  to its instance answer `503` — healthy co-subscribers included — until it
  drains, disconnects, or the instance is deregistered. `broadcast` offers no
  way to evict one subscriber without dropping the channel, and dropping it
  would discard envelopes the log has already recorded delivered; the
  per-subscriber buffer that would bound this is DOC-60 §7's durable inbox,
  deferred. Senders should retry with backoff, and an operator seeing repeated
  `503`s should look at the recipient, which the `warn!` names.
- `GET /api/v1/bus/subscribe/{instance_id}` reports a lag instead of swallowing
  it. The handler mapped `Lagged(n)` to `None`, borrowing an idiom from the
  session SSE handlers that carry load-sheddable telemetry — which DOC-60 §3
  keeps off this bus. It now emits an `event: lagged` frame carrying the missed
  count and the durable log's path to re-read from, plus a `warn!`.
- The bus's durable JSONL stream no longer interleaves two records into an
  unparseable line when two publishes land at once. `AuditLogger::try_log` wrote
  the record and its newline as separate unbuffered writes; it now appends both
  in one `write_all`.
- ADR-0036's "Related Decisions" cited "DOC-66 §3's per-worktree sentinel extension". The content is in §1.3; §3 is "Refreshing the tm checkout" and has no sentinel content.
- `.trusty-mpm-worktree` is now gitignored. The daemon writes it INTO a worktree, so it surfaced there as an untracked file and tripped the clean-tree guard in `scripts/check-publish-ready.sh`.
- A dispatched agent's worktree is now reclaimed when the session that dispatched it ends, not only when its `SubagentStop` arrives. `SubagentStop` was the reap's only trigger, so an agent killed by a session exit or restart, an interrupt, or a dropped hook POST left its worktree registered and owner-known — and the orphan sweep skips agent-owned trees by design, so nothing else ever reclaimed it.
- A reap that keeps a worktree is now logged at `warn!` rather than `info!`. The refusal is the only notice anyone gets that a tree needs a human, and nothing retries it.
- A session-end sweep now reports how many trees it removed, how many were already gone, and how many it kept. It reported two numbers before, counting a tree the harness had already reclaimed as one this sweep removed — which overstated the only number an operator reads to judge whether the reap is doing anything.
- **`/doctor` over HTTP no longer reports a healthy daemon as unreachable.** `DaemonClient::doctor` used the 10s client-level default while the handler runs the whole ~32-check battery inline — `gh auth status` alone measured 3.06s here, and the two worktree scans walk the managed workspace root. Measured against the default: 8 of 10 runs failed, every failure clustered at 10.2–10.3s and every pass at 9.6–9.7s, so Telegram `/doctor` and Slack `/doctor` answered "daemon unreachable" for a daemon that was still producing the report. The request now carries its own 120s bound, the same per-request override pattern the chat, provisioning and merged-PR-survey endpoints already use ([#5111](https://github.com/bobmatnyc/trusty-tools/issues/5111))
- `tm hook --pm-guard` no longer charges a read-only here-document command
  against the PM's per-turn file-change budget
  (closes [#5356](https://github.com/bobmatnyc/trusty-tools/issues/5356)).
  The redirection check treated any unquoted `>` as a file write, and its quote
  scan knows only `'` and `"` — so a `>` in here-document body text, such as a
  `len(k) > 3` comparison in a `python3 <<'PY'` script or an `->` arrow in
  `cat <<EOF` prose, read as a redirect. Three such reads were allowed silently
  while consuming the budget, and the fourth was denied as "PM file-change
  budget 3/3 used this turn" with zero files changed. Only the body is treated
  as data: `python3 <<'PY' > out.rs` is still a file write, still consumes the
  budget, and still exhausts it.
- The launchd probe no longer reads an unreadable `~/Library/LaunchAgents` as
  "no launchd unit is registered"
  (closes [#5623](https://github.com/bobmatnyc/trusty-tools/issues/5623)).
  `daemon_launchd_label_in` mapped every `read_dir` error to the same answer as
  an empty directory, and `tm start`, `tm restart` and the MCP stdio bridge all
  read that answer as permission to spawn — creating the unsupervised orphan
  daemon of #2486/#4230. It now answers in three states, and both spawn sites
  and `tm doctor`'s `daemon_orphan` check treat "could not determine" as
  "launchd may own this" rather than as absence. A home with no `LaunchAgents`
  directory still resolves to "no unit registered" (ADR-0045 §3).
- The `tm-issues-prune` skill no longer presents a truncated backlog as the
  whole one. Its scan and prioritize passes hardcoded
  `gh issue list --limit 500` against a repo with 745 open issues; they now
  paginate to exhaustion and report retrieved-vs-total, so a short read
  announces itself.
- `tm doctor`'s `asset_tier` check no longer prints
  `no tm-owned agent files outside the canonical tier … (scanned: <dir>)` for a
  directory whose scan failed (#5626, ADR-0045). A tier it cannot enumerate is
  reported as UNDETERMINED and downgrades an otherwise-clean run to `Warn`; the
  `scanned:` list now names only directories that were actually read. A
  confirmed shadowing still outranks it and keeps its `Fail`.
- Every `SkillManifest::load` call site now handles an unreadable ledger
  explicitly instead of proceeding on the empty default: the skill deploy, prune,
  retire, repair and adopt paths refuse to act, and the read-only probes
  (`skill_drift`, `skill_staleness`, `stale_skills`, `deploy_validate`,
  `doctor_scaffold_tracking`, `session_assets`, `update_check`) report the
  failure rather than "nothing is owned here".
- `tm session prune-worktrees --merged-prs --force` deleted live dispatched agents' worktrees. The merged-PR reclaim path never read the ownership sentinel: its liveness gate checks `SessionRecord.workspace_path`, which an agent has none of, and its ownership gate only asked whether a sentinel file EXISTS. A worktree carrying an agent sentinel on a branch whose PR had merged therefore passed every gate. `worktree_reclaim::classify` and `recheck_before_delete` now consult `SentinelOwner`, and reclaim an agent-owned worktree only when the delegation registry positively reports that agent's delegation ended — a registry that has no record of the agent refuses, because a delegation map rebuilt empty at daemon boot cannot tell a finished agent from a working one (ADR-0045). A sentinel inside `.claude/worktrees/` that is empty, malformed or unreadable refuses too, for the same reason. Session-owned `.worktrees/<name>` reclamation is unchanged. Closes #5661.
- Stopped four `spawn_disclaim` doc comments from linking into the macOS-gated
  `macos` submodule (`resolve_disclaim_fn`, `spawn_stderr_piped_disclaimed`,
  `spawn_stdout_piped_disclaimed`, `wait_for`), which rustdoc cannot resolve on
  Linux. docs.rs builds on Linux once per release and never rebuilds.
- `Explore` and `Plan` are no longer moved into their own worktree when dispatched from a main checkout. Both are read-only, and a granted worktree is cut from a commit — so they read a tree without the session's uncommitted work and answered about the wrong one. `general-purpose` is unchanged: it carries the full tool set and is also what a failed named-agent dispatch degrades into.
- The HEAD-move rule now keys its writer query by the main checkout's root as well as the directory the command resolves to. `cd crates/foo && git pull` keyed a subdirectory no delegation record was ever written at, so the query matched nothing and the move was allowed.
- `git merge -m "--continue" origin/main` is no longer exempt from the HEAD-move rule. The in-progress carve-out (`--abort`, `--continue`, …) scanned the whole argument list, so any command mentioning one of those strings exempted itself; it now matches the first argument only, which is where git accepts them.
- The HEAD-move denial attributes its claim to the daemon's delegation records instead of asserting that another agent is writing without a worktree. A record can outlive its agent, and a grant whose isolation report failed describes an isolated agent as unisolated.
- A dispatch denied because another agent already holds the main checkout now carries its own explanation instead of the concurrent-dispatch one, which offered `isolation: "worktree"` as the remedy — the exact rewrite the guard had already built and then declined to emit.
- The guard warns on stderr when it grants a worktree and the daemon records nothing. An empty writer list means both "the checkout is free" and "the daemon declined to record", and only the second leaves the delegation record uncorrected; a daemon older than the new route answers that way. The grant is still emitted, since failing closed on version skew would block every dispatch from a main checkout.
- The concurrent shared-worktree guard is no longer blind across sessions. `live_shared_tree_writers` filtered on the asking session first, so agents dispatched from different sessions into one checkout could not see each other — the exact shape of the reported incident. It is now keyed by directory, which is what a shared git HEAD belongs to.
- Regenerated `tm-capabilities/references/cli.md` and `mcp-tools.md`, which had drifted from `main` since v1.5.0 (#5769) added a daemon route and changed the guard surface. `mcp-tools.md`'s `session_new` entry now reflects the ADR-0037 reversal: a local `repo_url` runs the session on that main checkout itself, only a remote URL is cloned into a freshly-provisioned workspace.
- A daemon started under a throwaway `$HOME` no longer reaches the real tmux server. tmux is keyed to the OS user, not to `$HOME`, so a scratch daemon's startup auto-discovery still listed the operator's live panes, adopted them, and — because an adopted record carries the real project's working directory — refreshed `.claude/skills/` inside real project checkouts, one of them in a different repository. `TmuxDriver::discover` (the crate's only constructor for a tmux-backed driver) and `discovery::discover_all` (which also scans the host process table) now compare `$HOME` against the home the OS password database records for this uid, and refuse when the two differ. The check fails closed: an environment it cannot classify is refused too, and every refusal is logged with its reason. `TRUSTY_MPM_ALLOW_HOST_STATE=1` lifts it for anyone deliberately testing tmux adoption.
- `tm launch` and `tm connect` are covered too. They create and kill sessions through `core::tmux::create_managed_session` and `run_tmux` without ever constructing a `TmuxDriver`, so the check also sits at `run_tmux_with_bin`/`run_tmux_argv_with_bin` — the crate's single tmux-spawning point.
- A refused tmux path now says so. `daemon::services::tmux_service` returned `SessionNotFound` (HTTP 404, "that session does not exist") for a gated environment, and the reap and orphan-GC loops skipped silently every 60 s; all four now carry the gate's reason. `POST /sessions/discover` gained a `skipped` field, so a refused scan is no longer byte-identical to one that found nothing.
- The boot `inproject-hygiene` sweep is gated. `repos_root()` is only `$HOME`-scoped when nothing overrode it — `TRUSTY_MPM_REPOS_ROOT` and `TRUSTY_MPM_WORKSPACE_ROOT` outrank `$HOME`, and a scratch daemon launched from the operator's shell inherits those exports.
- The agent-worktree reap no longer removes a worktree the harness can still resume an agent into (#5800). A `SubagentStop` reports a turn boundary rather than an agent's exit, so it no longer triggers a reap; a session's end, which does prove its agents are gone, is now the sole trigger.
- A finished delegation's registered worktree is protected from another agent's reap — a terminal status records that trusty-mpm watched the delegation end, not that the harness released the directory.
- A worktree whose agent is stamped on two directories is kept rather than guessed at, so an ambiguous ownership claim reclaims neither tree.
- Session launch now resolves the palace it exports as `TRUSTY_MEMORY_PALACE` through `trusty_common::palace_resolve` rather than the pure `derive_palace_id` core. The core never reads `.trusty-tools/trusty-memory.yaml`, so the exported variable carried a git-derived name — and because the variable is the highest-precedence level, that name outranked the committed pin it was meant to lose to: this repo's sessions resolved `bobmatnyc-trusty-tools` while its pin said `trusty-tools`. Palace-alias registration also skips projects carrying a pin, since the aliased-from name is no longer resolved by anything ([#5811](https://github.com/bobmatnyc/trusty-tools/issues/5811))
- `tm session prune-worktrees --merged-prs` spared a live dispatched agent's worktree without saying so. The gate that protects it has refused since #5661, but a candidate blocked during classification appeared in none of the lists the prune route returns, so a run that protected an agent's tree printed `reclaimed 0 worktree(s)` — output identical to a run that found nothing to reclaim. The route now returns `spared_agent_owned`, and the CLI prints one line per spared worktree naming the agent it was spared for.
- `--merged-prs` help text said the pass "still refuses any worktree a session claims or that holds unsaved work", which reads as covering dispatched agents. It does not: an agent has no session record, and an agent that has committed and pushed has a clean tree. The text now states that a separate gate covers agent-owned worktrees, and that a delegation registry which cannot answer refuses.
- `tm session prune-worktrees --merged-prs` no longer fails with "operation timed out" on every invocation. The merged-PR survey runs synchronously inside the daemon handler and takes minutes — over 600 seconds byte-walking 46 worktrees on a large workspace — while the request inherited the client's 10-second default bound, so the CLI hung up before the daemon could ever answer. The request now carries a 1800-second per-request override (`RECLAIM_SURVEY_REQUEST_TIMEOUT`), and prints a one-line notice up front so a long silent wait is distinguishable from a wedged daemon. Only the `--merged-prs` opt-in raises the bound; the orphan-only sweep keeps failing fast at 10 seconds (Refs #5830).
- **The daemon-unreachable fallback's session now runs in the worktree it provisioned, not in the shared base clone** — `tm launch`'s managed-checkout redirect fired on every caller, including the ones that had already resolved placement themselves, so the fallback's fresh worktree and branch were abandoned and two concurrent fallback sessions landed in one tree. A caller that resolved placement says so (`LaunchDir::CallerResolved`) and the redirect is skipped
- Per-project `gh` account enforcement now confirms the identity with
  `gh api user`, not by parsing `gh auth status` text
  (refs [#5849](https://github.com/bobmatnyc/trusty-tools/issues/5849)).
  `gh auth status` echoes whatever the scoped `GH_CONFIG_DIR`'s `hosts.yml`
  claims, so a `hosts.yml` naming an account that exists nowhere on the machine
  printed a green checkmark, live token scopes and `Active account: true` — and
  `ensure_gh_account_in_dir` / `ensure_gh_account_for_project` reported success
  for an identity nobody had verified. Both now compare the expected account
  against `gh api user --jq .login`, the login GitHub attributes to the
  credential the scoped `gh` actually resolves, and every probe failure —
  `gh` missing, a non-zero exit, blank output, or the 5s bound — is a
  verification failure rather than a pass — including a lost network, which now
  blocks enforcement instead of passing it (every operation it gates needs the
  network anyway). A failed probe also stops before `gh auth switch`, so a blip
  cannot rewrite the project's config dir. Enforcement additionally refuses
  outright when an ambient `GH_TOKEN`/`GITHUB_TOKEN` is set: that token outranks
  `GH_CONFIG_DIR` for every later `gh` call, so no scoped probe can speak for
  what those calls will do — including a whitespace-only token, which `gh`
  selects rather than ignores. Enforcement also now runs on the project's
  configured `github.host` instead of assuming github.com, so a project pointed
  at an enterprise instance is verified on the host its work actually uses;
  `configured_account_pair` returns an `AccountTarget` carrying that host
  alongside the config dir and account. When the transcript and the
  credential name different accounts (the divergence tracked in
  [#5656](https://github.com/bobmatnyc/trusty-tools/issues/5656)) the API
  answer decides and the disagreement is logged at WARN.
- A session in a project pinned to a non-default `gh` account no longer runs as whoever is globally active. Spawn used to mint its credential with `gh auth token -u <account>`, and on a keyring-backed host that flag does not discriminate — `-u bobmatnyc` and `-u bob-duetto` return the identical value. A project that sets `github.config_dir` is now pinned with `GH_CONFIG_DIR`, which does select the account, and no `GH_TOKEN` is emitted alongside it (an env token outranks the scoped config in `gh`'s own resolution order). A project with no `config_dir` keeps the previous token-minting behaviour. #5851
- A pinned `gh` config dir that holds no credential stays pinned and logs a warning naming the directory and the `GH_CONFIG_DIR=<dir> gh auth login` that fixes it. Falling back to the machine-global account would be the wrong-identity bug itself. #5851
- A live, attached session is no longer decommissioned at daemon boot because tmux could not be reached. `dedup_stale_duplicates` built its live-session set with `list_sessions().unwrap_or_default()`, and `plan_workspace_duplicates` reads an empty live set as proof every candidate record is dead — it picks a survivor by recency and decommissions the rest. `decommission` is irreversible: it tombstones the record, clears `workspace_path`, and the picker hides `decommissioned` rows, so the operator's own session vanished from the picker while they sat in the pane. The liveness query now runs the `ensure_server_up` guard and propagates `list_sessions()`'s error instead of defaulting it; a failure to observe tmux logs a warning and skips the whole pass, decommissioning nothing.
- `reconcile_on_boot` no longer marks a running session `Stopped` because tmux could not be reached. It built its live set with `list_sessions().unwrap_or_else(warn, Vec::new)` — the same fail-open shape, forty lines above the dedup call it feeds — so an unobservable tmux sent every live record down the "session is gone" arm, and under `auto_resume` queued each one for a relaunch it did not need. It now routes through the same refusing liveness query and skips every liveness-derived decision when tmux cannot be observed: no record changes state, nothing is adopted. The skip is not an early return — the dedup, deploy-validate and auto-resume tail still runs.
- `NoopTmuxDriver::list_sessions` reports `TmuxUnavailable` instead of zero sessions. The daemon installs that driver exactly when `RealTmuxDriver::discover()` failed — tmux off PATH, or the #5784 host-state gate refusing tmux access on a reassigned `$HOME` — so it has never asked tmux anything, and `Ok(vec![])` made "tmux is unreachable" byte-identical to "tmux is running zero sessions". No error handling at the call site could catch that, because there was no error. The remaining consumer, the trait's `session_exists` default, maps `Err` to `false` and is unchanged (#5859).
- `reconcile_on_boot` now reports a terminal record whose tmux session is live, naming the reactivate call that resolves it. It does not auto-revive: the contradiction cannot be told apart from the #2777 case of a correctly decommissioned session whose pane lingers as a bare shell, and `decommission` has already cleared `workspace_path`, so a silent revive would restore a record the picker keys on incomplete fields.
- A tmux liveness probe that fails now refuses `tm session delete`, `tm session
  prune`/`decommission`, and `tm session resume` instead of reading the session
  as dead. Previously a transient `list-sessions` failure (or the no-op driver
  installed when tmux cannot be discovered) let an unforced prune tear down a
  live session and let resume kill and rebuild a live pane (#5859).
- `tm doctor`'s `skill_staleness` check now states the drift remedy per deploy tier. It claimed every finding was "REPAIRABLE by `tm install`", but `tm install` writes only the operator-home tier — so an operator re-ran it against managed-config and project-tier drift and watched the count refuse to move. Managed-config and project-tier drift now name the writer that actually refreshes them, plus `tm doctor --fix-skills`. Severity is unchanged: conventions-bearing drift still escalates to `Fail` at every tier ([#5865](https://github.com/bobmatnyc/trusty-tools/issues/5865))
- The `output_style_staleness` and `output_style` checks no longer tell the operator to run `tm install`, which has no output-style step — the deployed file kept its mtime across a full install run. Both now name `tm doctor --fix --yes`, and the drift scan behind the report is the same one the repair acts on ([#5866](https://github.com/bobmatnyc/trusty-tools/issues/5866))
- `tm doctor` no longer treats an arbitrary working directory as a managed workspace. `run_doctor` applied the workspace path layout to whatever `project` the CLI sent, which pointed the operator-home skill tier at `<cwd>/.claude/skills` — the same path as the project tier — so the deploy-tier dedup dropped one of them, `~/.claude/skills` went unaudited, and project-tier findings were reported under the "operator home" label. The workspace layout now applies only to a directory a live session was provisioned into ([#5867](https://github.com/bobmatnyc/trusty-tools/issues/5867))
- One output style that cannot be read no longer makes a different style report `Failed` after `tm doctor --fix` correctly rewrote it. `deploy_output_styles` aborted the whole batch on the first IO error, discarding every write it had already completed in that call, so the repair reported a file it had just refreshed to bundled content as a failure — the same report-contradicts-disk defect [#5865](https://github.com/bobmatnyc/trusty-tools/issues/5865) is about, reintroduced by the [#5866](https://github.com/bobmatnyc/trusty-tools/issues/5866) fix. Each style now carries its own outcome: one that was written reports written, one that could not be read reports its own reason, and neither contaminates the other ([#5874](https://github.com/bobmatnyc/trusty-tools/pull/5874))
- `tm session prune` now frees the slot number behind every tombstone it compacts, so `NUM` stops climbing. `prune_managed`'s compaction branch removed the record from the store and left the slot registry still holding its number; `numbered_snapshot` walks the slots the registry HOLDS and tombstones any whose record has vanished, so each pruned session kept rendering as a `-- deleted --` row and its number was never handed out again. One machine accumulated 44 such rows with new sessions still being assigned into the 130s. `SlotRegistry::release` had exactly one caller, the retention sweep, which is why only sessions whose worktrees had already left the disk ever got their numbers back. The release is gated on the same `workspace_needs_protection` predicate that sweep uses, so a record whose `.worktrees/<uuid>` directory and `.trusty-mpm-worktree` sentinel are still on disk is compacted but keeps its slot — handing that number to a newly-spawned session whose worktree already exists is worse than a cosmetic tombstone row, and the shared predicate keeps the two paths from drifting apart again. Compaction remains a store operation: it removes no directory, and a dry run releases nothing. Both conditions are read from the record the store holds at the moment of removal, not from the snapshot prune took before its loop: a session revived in place while that prune was still tearing other sessions down keeps its number rather than having it handed to the next spawn.
- `tm session prune` no longer deletes the store record of a session that was reactivated while the prune was running. The reload that decides whether the slot is safe to free now decides the removal as well; previously it gated only the slot, and the record was removed unconditionally before that check ran. A session revived through `tm session resume` mid-prune correctly kept its number and still lost its record, so `tm session status` and every lookup by id answered `SessionNotFound` for a live, running session and `tm ls` rendered it as a `-- deleted --` phantom. A record that is no longer a tombstone at reload time now keeps both its record and its slot, and prune omits it from the reported outcome rather than claiming it was removed.
- `tm session decommission <id>` no longer reports "workspace removed" when the
  workspace is still on disk
  (closes [#5899](https://github.com/bobmatnyc/trusty-tools/issues/5899)).
  The daemon's `workspace_removed` verdict was dropped client-side — the response
  decoded into `ManagedSessionSummary`, which has no field for it, so serde
  discarded it silently — and the CLI printed a hardcoded success line for every
  decommission, including adopted and local-path workspaces it never touched. The
  verdict now rides a purpose-built `ManagedDecommissionOutcome` through the
  executor to one shared message helper: it reports removal only when the daemon
  says the removal happened, says the workspace is still on disk when it says
  otherwise, and says the outcome is unreported when an older daemon sends no
  verdict. `tm ls` tombstone rows also need
  [#5897](https://github.com/bobmatnyc/trusty-tools/issues/5897) to clear.
- `tm hook --pm-guard` no longer admits a concurrent shared-worktree dispatch
  when the daemon fails to answer
  (closes [#5923](https://github.com/bobmatnyc/trusty-tools/issues/5923)).
  `post_shared_tree` returned the same "nobody else is here" for all five of its
  failures, so a request timeout, a 5xx and an unparseable body allowed the
  dispatch the guard exists to deny — and the 2 s budget is reachable on an idle
  machine, which put the guard off under exactly the load that makes two
  concurrent dispatches likely. The failures now split by what they say about
  the daemon: a daemon that is listening and does not answer usably DENIES, with
  a message naming the failure and the remedies that need no daemon; a daemon
  that is not listening, or that 404s the route because it predates it, still
  allows and now says on stderr that the guard is not enforcing.
- The ADR-0048 worktree grant applies the same split. That call computes the
  #4480 concurrency verdict before the grant rather than skipping it — "a
  dispatch made into a checkout another writer holds is denied, and only an
  empty answer is granted" — so a running daemon that leaves it unanswered now
  denies there too. The grant it would otherwise emit is a rewrite of the
  dispatch's arguments that `tm` cannot confirm the harness applies, and in a
  main checkout ADR-0048 states the cost: a false allow corrupts another
  session's branch. An absent daemon still grants, and now warns on stderr that
  the check did not run.
- The read-only writer query behind the `git merge` / `git rebase` /
  docs-commit rules keeps its fail-open contract. A deny there blocks ordinary
  git work on the operator's own checkout rather than admitting a second agent.
- `tm hook --pm-guard` now denies a `git worktree add` under
  `/private/var/folders`, the spelling macOS's `/var` symlink resolves to
  (refs [#5924](https://github.com/bobmatnyc/trusty-tools/issues/5924)).
  The denylist carried `/var/folders` only, and `current_dir()` inside
  `$TMPDIR` returns the `/private/var/...` form — so a worktree target given
  relative to a `$TMPDIR` working directory matched nothing and the guard let
  it through. `/tmp` and `/private/tmp` were already paired this way; `/var`
  now is too.
- `tm project list` reads the persistent project registry
  (`GET /api/v1/projects`) instead of the daemon's in-memory, session-derived
  project map (refs
  [#5994](https://github.com/bobmatnyc/trusty-tools/issues/5994)). That map is
  rebuilt on every daemon start and seeded only from `config.yaml` and session
  history, so the command reported "no projects registered" on a host whose
  `projects.json`, `project_list` MCP tool, and `/api/v1/projects` route all
  listed five. Rows now render like `tm projects list` (name, repo URL,
  default branch) and keep the `[mcp-trusted]` marker, resolved through the
  local project-path alias store.
- `tm doctor`'s `worktrees` check counted every git-registered worktree no live session claimed as an orphan, reporting 198 where `tm session prune-worktrees` and `tm session reconcile-worktrees` both found zero. It now reports the ORPHANED count from the reconciled inventory those two verbs already share, so an agent-owned, locked, dirty, or sentinel-less worktree is no longer called an orphan (#5947).
- That check's remediation hint named `tm session prune --worktrees`, a flag that does not exist — `tm session prune` takes `--state` only. It now names `tm session prune-worktrees`, and a test parses the hint against the real CLI so a renamed flag cannot leave it pointing at nothing (#5947).
- Decommissioning a tm-owned worktree through the MCP `session_decommission`
  tool or the idle reaper no longer leaves a stale entry in `git worktree list`
  (refs [#5949](https://github.com/bobmatnyc/trusty-tools/issues/5949)). Both
  call `SessionManager::decommission` in-process, below the client layer that
  carried the repair for the routed HTTP paths, so the owned-workspace branch
  removed the directory with `remove_dir_all` and git kept listing it — the
  reaper unattended. The repair now runs inside `decommission_with_root`, which
  every caller reaches regardless of transport, and it targets the checkout git
  itself reports as owning the worktree rather than the parent directory.
- `tm services` reports the PID of the process bound to a service's port, not
  the first `pgrep -f` name match
  (closes [#5951](https://github.com/bobmatnyc/trusty-tools/issues/5951)).
  The manifest's `process_match: "trusty-mpm"` is a substring every sibling
  process shares, so `trusty-mpm-daemon` was reported with a
  `trusty-mpm serve --stdio` bridge's PID while `launchctl list`, `ps`, the
  daemon's own `/health` and the console gateway all named the daemon. Since
  this repo's guidance sends operators to `tm services` instead of raw
  `ps`/`lsof`, that PID is one somebody signals. Name matching remains the
  fallback for portless services such as `trusty-embedderd`, for a port nothing
  is listening on, and for a host without `lsof`.
- `tm services status`, `list`, `port`, `url`, `health`, and `log` no longer
  panic before printing anything in a debug build
  (closes [#5965](https://github.com/bobmatnyc/trusty-tools/issues/5965)).
  `RealHttpProber::get_health` built a `reqwest::blocking` client directly on
  the caller's thread, and `tm`'s `main` is `#[tokio::main]` — so every health
  probe constructed reqwest's internal runtime inside the async runtime and
  aborted with "Cannot drop a runtime in a context where blocking is not
  allowed". Release builds only appeared healthy because the guard that raises
  that panic is compiled in under `debug_assertions`; the call still parked the
  thread it ran on from inside `poll`. The client now runs on a dedicated OS
  thread that is joined for its result, matching
  `trusty_common::search_index::best_effort_create_index`. `init` and `restart`
  were unaffected — they never probe.
- The `claude --version` probe behind output-style detection now gives up after 5 seconds, killing and reaping the probe child, instead of waiting forever — a wedged `claude` binary used to hang session launch and every prompt-composition path and leave orphaned probe processes behind (#5969).
- `tm launch` no longer walks the entire home directory before starting a
  session ([#5875](https://github.com/bobmatnyc/trusty-tools/issues/5875),
  [#6070](https://github.com/bobmatnyc/trusty-tools/issues/6070)). Step 8 calls
  `remove_global_trusty_mpm_hooks`, which reached `~/.claude/settings.json` and
  `~/.claude/settings.local.json` by recursing eight levels into `$HOME`. On a
  real developer machine one `opendir()` inside that walk blocked indefinitely,
  so the launch never returned; a stack sample showed 2328 of 2328 samples in a
  single `open$NOCANCEL`. The strip now names the two global files directly.
  Project settings files elsewhere under `$HOME` are no longer rewritten as a
  side effect of launching — `tm hooks clean` owns that machine-wide sweep, the
  same split `tm install` adopted in #2940.
- The eight `guided_fallback_*` tests in `--bin tm` terminate again. They drive
  `launch()` end to end, so they inherited the hang, and because they hold the
  process-wide `serial_test` lock while stuck they starved every other
  `#[serial]` test in the binary — which is why `cargo test -p trusty-mpm`
  needed `--skip guided_fallback` to complete at all. That workaround is no
  longer required.
- `remove_global_trusty_mpm_hooks_at` takes the home directory as a parameter,
  which gives the removal path its first real test coverage. Four doc comments
  cited `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`, a test
  that did not exist.
- The Session Manager no longer sends a model id to a provider that has never
  heard of it. A tier model with no routing prefix took the configured
  `provider` whatever it looked like, so a `us.anthropic.*` inference profile
  under `provider = "openrouter"` — or an OpenRouter slug under an `auto` chain
  that picked Anthropic — went upstream unchanged.
  `core::sm::providers::resolve_provider_and_model` now infers the provider from
  an unambiguous id shape before the default or the credential precedence chain
  gets it, and returns `SmLlmError::Validation` naming both the id and the
  provider when they disagree. Credentials decide which provider is reachable,
  never which model the operator asked for. An explicit `anthropic/` /
  `bedrock/` / `openrouter/` prefix still wins, is still stripped before the
  shape is read, and is overruled only by a shape that belongs to exactly one
  provider's catalogue — never by the dotted `vendor.model` guess, so
  `openrouter/anthropic.claude-x` keeps working. The function is now fallible.
- A rejected model id no longer suggests a `provider` value the config parser
  refuses. When the id belongs to a provider the SM has no client for, the error
  says so and names the accepted set instead of telling the operator to set
  `provider = "fireworks"`. `ProviderKind::parse`, its error, and the
  shape-mismatch remedy all read one list (`ProviderKind::ALL`), so the accepted
  values and the routing-prefix hint cannot drift from what the parser takes.
- The daemon's boot adoption sweep no longer adopts a tmux session named under
  the reserved test namespace `tm-xtest-` (#6116). A test that spawns a real
  tmux session leaks it whenever the test process dies without unwinding — a
  SIGKILL, a `cargo test` timeout, an aborted run — so PR #6125's kill-on-drop
  guard, which covers the panic path, cannot cover that one. Three sessions
  leaked that way on 2026-08-24, adoption turned each into a managed record, and
  the session picker grew three `(active)` ghosts that outlived the tmux
  sessions themselves. The refusal decides from the name alone, so it holds for
  every leak shape whether or not any test-side cleanup ran, and it also returns
  the leaked pane to `daemon::orphan_gc` — an untracked idle shell with no live
  child is killed there after two sweeps, whereas being adopted made it
  permanent.
- An ADOPTED reserved-namespace record is now tombstoned by boot
  reconciliation, whether its pane is live or gone, and no automatic path
  resumes one (#6116). A record adopted by a daemon build predating the refusal
  survives in the durable store, and leaving it alone had two outcomes: a gone
  pane left a `Stopped` picker row nothing could resume — the state that forced
  a manual `session_delete force` — while a LIVE one sustained a loop, since
  re-adopting it restored its orphan-GC immunity, the reaper then stamped it
  auto-resumable, the supervisor recreated the tmux session, and the next boot
  re-adopted it. The daemon log shows that cycle three times in one day. Both
  rules ask for an adopted provenance as well as a reserved name, so a session
  the daemon CREATED for a project named `xtest-…` keeps ordinary stop, resume
  and list behavior. An ordinary session's gone-tmux handling is unchanged:
  still `Stopped`, still resumable.
- The boot-reconcile summary log now counts refused and swept test sessions, so
  a boot whose only finding was one of those no longer prints nothing.
- The `#3873` dead-runtime fixture now mints its real tmux session inside that
  namespace, and the test-support guard sweeps reserved-namespace sessions older
  than 30 minutes once per test process, so the next run on a machine reaps what
  a hard-killed run left behind. Neither mechanism survives a SIGKILL on its
  own; the daemon-side refusal is what makes a leak harmless while it lasts.
- Boot reconciliation adopts a live tmux pane under an id derived from the
  pane's tmux name, so one pane can only ever occupy one store row
  ([#6117](https://github.com/bobmatnyc/trusty-tools/issues/6117)). The
  external-adopt loop decides from a snapshot of store names taken once, before
  the loop; a second adopter holding its own copy saw the same pane as unknown
  and wrote a second record under a fresh random id. The store is keyed by id,
  so both survived — 11 tmux names carried two records apiece in the reporting
  store, several pairs written 30-60 ms apart. A name-derived id makes the
  second write land on the first's key instead of beside it.
- Boot reconciliation no longer adopts a pane whose working directory it cannot
  resolve ([#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118)). Such
  a pane used to become an `Active` record with a `/unknown` cwd and an
  `unmanaged` note in `task`, and that record could never be retired: the
  `tm ls` auto-prune keeps any record whose tmux name is live, and the orphan-GC
  keeps any pane a registry names — so adopting the pane is precisely what made
  it permanent. 55 of the 103 records in the reporting store were these. The
  pane is now left untracked and named in the daemon's boot log, which hands it
  to the orphan-GC: it kills a managed-prefix pane only when the pane is an idle
  shell with no live child process, seen idle on two consecutive sweeps, and it
  keeps one still running an agent. Adoption of a pane whose cwd does resolve is
  unchanged.
- The working-directory probe behind that decision retries before giving up. It
  is a kill decision now — a declined pane is orphan-GC input — and
  `TmuxDriver::pane_current_path` reports a failed spawn, a non-zero exit and
  empty output all as `None` with no retry, while boot reconciliation runs once
  per daemon process. One flaky answer would have cost a live pane inside two
  minutes.
- `ReconcileReport` carries `adoption_declined`, the tmux names reconciliation
  declined this pass, and the daemon's boot summary logs the count and fires on
  it, so a pane that stops appearing in `tm ls` is accounted for rather than
  silently absent.
- **`PruneFilter::Unresolvable`'s doc comment now links to a symbol that actually exists.** The link pointed to `super::record::SessionRecord::workspace_unresolvable`, but `prune_types.rs` loads as the `types` submodule of `prune` (via `#[path = "prune_types.rs"]`), so `super` resolved to `prune`, which has no `record` submodule — `cargo doc`'s broken-intra-doc-link gate failed deterministically. The link now uses the crate-absolute path `crate::session_manager::record::SessionRecord::workspace_unresolvable` ([#6118](https://github.com/bobmatnyc/trusty-tools/issues/6118))
- No prune path can select the session it was invoked from. The CLI now sends
  `$TM_MANAGED_SESSION_ID` as `invoking_session`, and the prune engine excludes
  that record for every filter, `--include-active` included. A present but
  unparseable value is rejected with 400 rather than dropped, so the sweep never
  runs with the self-exclusion silently switched off.
- **A session you stop or kill stays stopped.** `Stopped` said only that the runtime was not running, so both automatic relaunch paths — the supervisor's per-tick sweep and boot reconciliation — treated an operator's `tmux kill-session` and `tm session stop` as sessions to revive; two leaked sessions killed at the tmux level came back within seconds, and only `tm session decommission` (which deletes the workspace) made them stay down. Every transition into `Stopped` now records why. `tm session stop`, the HTTP and MCP stop routes, and the idle auto-stop record a deliberate stop, as does the reaper when it finds one session's tmux target gone while the tmux server is still up — and the automatic paths leave those records alone. Everything that cannot be attributed to a decision still auto-resumes: a runtime that exited on its own, a session whose tmux target vanished while the daemon was down, and the loss of the whole tmux server (`tmux kill-server`, a crash, a logout), which `tmux list-sessions` reports the same way as an empty host and which must not strand the entire fleet. The post-reboot fleet restore is unchanged. `tm session resume` still revives a deliberately stopped session — the gate is on automatic resume, not on resume. Records written before this field existed load unchanged and keep the auto-resume behavior they had ([#6194](https://github.com/bobmatnyc/trusty-tools/issues/6194))
- The orphan-index sweep no longer reads a failed status probe as an empty
  index. It substituted `chunk_count: 0` for a refusal or an unanswered call, and
  `0` is exactly the reading that makes an aged, unclaimed `.worktrees` index
  collectable — so a wedged or restarting daemon could license a delete on
  evidence that was never gathered. A candidate whose chunk count cannot be read
  is now skipped, and a listing that goes unanswered ends the sweep with nothing
  collected rather than erroring.
- A destructive index delete is reported as removed only when the daemon says the
  registration is gone. Any success answer used to count, including the #3049
  body that reports `removed: false` because an in-flight writer never quiesced —
  so the sweep recorded an index as reclaimed while it was still registered and
  still on disk.
- **The health screen's PALACES panel silently dropped a palace it could not read.** `memory_collections` fanned out one `memory.palace_get` per id and skipped any that failed, so the panel could list fewer palaces than the daemon has with nothing saying which or why. It makes one `memory.palaces_list` call now; a row carrying an error renders with `✗` and the daemon's reason, exempt from the empty-palace filter — a palace whose counts could not be read is not a palace holding nothing ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286))
- `console_metrics` and `supervisor_status` now report the supervisor's real `run_stats` — sweeps, auto-resumes, resume failures, classifications — instead of a permanent zero. A missing, corrupt, or stale snapshot is reported in a new `supervisor_metrics` field rather than as a silent zero. Staleness is measured against the supervisor's own configured sweep interval (three sweeps, floored at 300s), so a slow overnight cadence is not reported as a dead supervisor (Refs #6288).
- **`tm doctor` is a standalone diagnostic and no longer needs a daemon.** It fetched the whole report from the daemon's `GET /api/v1/doctor`, so with nothing running it printed `doctor failed: daemon unreachable: …` and ran none of the 27 purely local checks — it refused to diagnose at exactly the moment an operator needs a diagnosis. The battery now runs in-process through `daemon::doctor::run_doctor_for_manager`, which the HTTP route also calls, so both surfaces report the same checks. Daemon reachability is one appended row — `trusty-mpm daemon: reachable / not running / unresponsive` — computed by a bounded probe of the URL the CLI already resolved through gateway/lock-file discovery, never a hardcoded port and never a start. The `daemon_version` (#2332) and `daemon_orphan` (#4230) checks are comparisons about a daemon's answer, so with no answer they are skipped rather than reported. The "port 7880 unreachable" wording is gone ([#6336](https://github.com/bobmatnyc/trusty-tools/issues/6336))
- A daemon isolated by its data root alone no longer reaches the real tmux server. The #5784 gate compared `$HOME` against the OS password database and nothing else, so a `DaemonState` built with a scratch framework root and the operator's real `$HOME` left untouched classified as the real environment: `session_manager()` resolved a real tmux driver, and `reconcile_on_boot` adopted the operator's live panes into a throwaway store — each adopted record carrying a real project directory, which is how a test fixture deployed assets into real worktrees. On the machine that reproduced it, 250 live sessions were reachable that way. `host_state_gate::host_state_access_for_root` adds the caller's data root to the comparison, and every daemon path that holds that root now asks it: `DaemonState::session_manager`, `discovery::discover_all`, and — through the new `daemon::host_state_refusal` choke point — the `reap_loop` and orphan-GC background sweeps and the shutdown-time legacy reap. Those three ran against the operator's real tmux server every tick, and the shutdown reap had no host-state gate at all, so a scratch daemon's shutdown could `kill_session` a live operator session that merely shared a name. A refused session manager gets the same no-op driver the missing-tmux path gets, which reports tmux unobservable rather than empty, so reconciliation leaves every record alone. `TRUSTY_MPM_ALLOW_HOST_STATE=1` lifts this arm exactly as it lifts the `$HOME` one.
- `git worktree remove --force` is now built by one hardened helper that strips the environment variables pointing git at a different repository. `git worktree` resolves its repository from `GIT_DIR` ahead of its own `-C`, and the removal was the last git call in the agent-worktree reap and session-decommission paths still spelt out with a bare `Command::new("git")` — every gate ahead of it already went through the hardened builder. Under an ambient redirect (a git hook exports `GIT_DIR` and `GIT_INDEX_FILE` to everything it launches) the gates examined the real worktree, the removal resolved a different repository and exited 128 with `is not a working tree`, and the directory stayed on disk with the refusal reaching nothing but a `warn!`. A timed-out reap assertion now also names the ambient redirects instead of reporting only that the directory is still there ([#6391](https://github.com/bobmatnyc/trusty-tools/issues/6391))
- The tmux reap-parity test no longer predicts reap's behaviour from whether the
  `tmux` binary resolves
  (refs [#6411](https://github.com/bobmatnyc/trusty-tools/issues/6411)).
  `DaemonState::reap_dead_sessions` reaps nothing when `list-sessions` fails, and
  on a host where no tmux server has ever run that listing fails with
  `error connecting to <socket> (No such file or directory)` — a string
  `TmuxDriver::list_sessions` deliberately does not classify as an empty server,
  so a registry is never wiped by an unreachable tmux. `TmuxDriver::discover()`
  succeeds on that same host, because the binary is installed, so gating on it
  expected a removal that could not happen and turned `main` red. The probe now
  runs the listing itself.
- The `no server running` / `no sessions` stderr classification is one named
  function, `stderr_means_empty_server`, instead of the same pair of `contains`
  calls at three listing sites, so the boundary between an empty server and an
  unreachable one cannot drift between them.
- Boot reconciliation no longer sleeps through a retry that cannot change its
  answer. A live tmux pane whose recorded working directory has been deleted — a
  reaped agent worktree — was probed three times with the full 200 ms backoff
  before being declined, even though tmux keeps reporting the same recorded path
  on every probe. On a host with 276 such panes that was 55 s of sleep inside
  `reconcile_on_boot`, which `DaemonState::session_manager()` awaits before it
  hands the manager to its first caller; the SessionEnd worktree sweep is often
  that caller and reached its first gate a minute late. The retry still runs in
  full when tmux returns no answer at all, which is the transient failure it was
  added for (#6118), and no pane's verdict changes — measured on 294 live
  sessions, 60.65 s to 2.12 s (#6414).
- **A restored tmux server gets tm's scrollback settings back.** A tmux server
  restart with tmux-continuum restore recreates every `tm-*` session through
  tmux-resurrect's own bare `new-session`, which never passes through tm's
  session-creation choke point — so the server carried none of the
  `history-limit`, `mouse` or `alternate-screen` options tm specifies, and
  restored panes sat on tmux's factory 2000-line scrollback. The daemon now
  re-asserts those server globals when it reconciles sessions at boot, and a new
  `tmux_options` doctor check reports drift between the live server and tm's
  spec — warning per option, and reporting UNKNOWN rather than OK when no option
  could be read. A green check means NEW panes will be correct: `history-limit`
  is captured when a pane is created and cannot be grown in place, so an
  affected session still has to be restarted
  ([#6469](https://github.com/bobmatnyc/trusty-tools/issues/6469))

### Changed

- **`git pull` is no longer denied in a main checkout** — `tm hook --pm-guard` classified `pull`, `merge` and `rebase` alike as HEAD moves and refused all three beside another live writer, so the only update path it left was `git fetch`, which never advances local `main`. A pull now runs whether or not another session shares the tree, and it never reaches the daemon's live-writer query. `git merge` and `git rebase` are unchanged and still denied there — either can leave the shared tree conflicted or rewritten. Source edits and source commits in a main checkout are unchanged and still denied (ADR-0053, amending ADR-0048 decision 10)
- The daemon lock file carries a `product = "trusty-mpm"` field and readers
  reject any record without it
  (see [#1731](https://github.com/bobmatnyc/trusty-tools/issues/1731)).
  `daemon.lock` is not a unique filename — Claude Code writes one too, at a path
  containing "trusty-mpm" — and a reader that trusts the path alone can hand an
  operator an unrelated PID. A daemon started by an older binary writes no such
  field, so its record is ignored until that daemon restarts; discovery falls
  back to the console gateway probe and the default URL meanwhile, and
  `tm daemon` now also probes the address it is about to bind so it cannot spawn
  a duplicate during that window.
- The orphan-GC no longer reports agent worktrees as owner-unknown, and still never deletes them. A `Known` sentinel owner is resolved through a session-store lookup, which an agent has no record in — so resolving agent worktrees that way would report every one of them reclaimable once past the 10-minute grace window, and that sweep's chain carries no liveness check. Agent-owned is a third answer: attributed, and reclaimed only by the agent's own exit. They stay visible: `OrphanSweepOutcome` gains `agent_owned`, the prune HTTP route gains `agent_owned_paths`, and the daemon logs the count separately, so attributing a directory never costs an operator the view of it. `tm doctor`'s reconcile report names the owning agent instead of saying no sentinel exists.
- A sentinel write that fails now registers nothing. The in-memory record is what grants the reaper authority to run `git worktree remove --force`; writing it after a failed sentinel write would hand out deletion authority with nothing on disk backing it. Declining leaves the tree owner-unknown, which nothing auto-deletes, and registration is retried on the agent's next tool call. It is never fatal to the hook.
- The reap's in-use set now covers every session's non-terminal delegations, not just the stopping agent's own session. A session is not an isolation boundary for "is an agent still working in this directory" — a sibling dispatched from another session is exactly as live, and the narrower read let a reap approve a path that sibling held.
- `find_agent_worktree` returns a directory only when exactly one sentinel names the agent. Nothing deletes a stale sentinel and registration re-fires on every tool call, so one `agent_id` can come to name several directories; returning the first `read_dir` hit made the deletion target depend on filesystem enumeration order. Ambiguity resolves to none (ADR-0045).
- `tm-workflow`'s escape hatch for "I need a clean tree to run one command" is
  now a throwaway worktree
  (`git worktree add .claude/worktrees/baseline-$$ origin/main`) rather than
  stashing the main checkout and restoring it afterwards. The
  worktree needs no main checkout, and a command that dies partway through
  leaves every other tree as it was instead of stranding uncommitted work in a
  stash entry someone has to find (#4730).
- The bundled `git-workflow` skill gained the same throwaway-worktree recipe for
  getting a temporary clean tree, and its "Stashing Work" section now saves
  under a named ref and restores by that ref rather than a bare `git stash`
  followed by a blind `pop` (#4730).
- **The batch asset-staleness path takes its framework base as an argument.** `FrameworkPaths::for_managed_workspace_under`, `session_assets::session_plan_under` and `managed_routes::summary::stale_assets_for_many_under` accept the directory the framework install nests under; the existing entry points pass `FrameworkPaths::home_base()` and behave exactly as before. `core::tmux::create_managed_session_with_options` does the same for the resolved `tmux:` options. Both remove a `$HOME`-derived read that only tests needed to redirect ([#5040](https://github.com/bobmatnyc/trusty-tools/issues/5040))
- A second session launching on the same main checkout logs at `info` rather than `warn`, and `session_new`'s tool description no longer claims every session gets a freshly-cloned workspace. With writers isolated and the write boundary enforced, sharing a read-only checkout is expected rather than hazardous.
- `tm hook --pm-guard` now decides a `git commit` in a main checkout on what is STAGED rather than on the verb (ADR-0049). A staged set of documents and configuration commits there, provided the daemon reports no other live writer sharing that HEAD; any staged source file denies, and the refusal names the offending paths. A commit whose staged set does not describe it keeps the previous unconditional deny: nothing staged, an unreadable index, `-a`/`--amend`/`--include`/`--only`/a pathspec, or a command that chains anything but `cd` — one index read describes one commit, and only when nothing between the read and the commit can restage. The index is read with `diff.relative=false` and `--no-renames` so a subdirectory `cwd` and a staged `git mv` of source into a document cannot hide the source side. `git add` and `git mv` stay ungated: neither moves a ref. #5782, #5781
- `tm hook --pm-guard` now denies a dispatched agent's `git worktree remove` and names the PM's replacement: `tm session prune-worktrees --merged-prs --force`. Removing a merged worktree could not be delegated at all — an unisolated dispatch is denied as a second writer on the shared HEAD (#4480) and an isolated agent's git operations are confined to its own worktree, so it cannot act on the shared registry — while `BASE-AGENT.md` told every agent to do it anyway. The owner ruled on 2026-08-19 that the PM confirms the work is done and runs the removal itself, and the guard now enforces that. The PM is never denied, `git worktree list` and `git worktree prune` are never denied, and an indeterminate caller context fails open. `tm-workflow`, `git-workflow`, and `rust-build-performance` carry the same rule; `rust-build-performance` also stops calling merged-PR reclamation unshipped, which it has been since #2919 (Refs #5791).
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. The trusty-memory client is a separate case: `trusty_common::mcp::memory_rpc` became `trusty_common::memory_rpc`, still reached through the `catchup` feature. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
- `fetch_managed_session_until_stopped` (bare-`tm` in-place relaunch) takes its
  retry budget as a parameter instead of reading `FETCH_RETRY_BUDGET` directly.
  The one production call site passes the same 400ms, so the in-place relaunch
  path behaves exactly as before; the parameter exists so the retry tests can
  drive a budget no loopback round trip can consume, which is what made
  `fetch_until_stopped_gives_up_after_budget_when_never_stopped` flaky under CI
  load (#5840).
- The `tm projects` TUI's `[d]` decommission hotkey now routes through the same
  shared implementation as every other entry point, so it too repairs the base
  repo's worktree bookkeeping. It called the daemon endpoint directly and left a
  stale `git worktree list` entry behind — the same bug the rest of #5913 closes.
- Every decommission entry point now routes through one implementation,
  `CommandExecutor::decommission_managed_id`
  (closes [#5913](https://github.com/bobmatnyc/trusty-tools/issues/5913)). The
  bulk prune sweep used to reach the endpoint through its own hand-rolled
  `reqwest` POST, independent of the routed `tm session decommission <id>` verb;
  that split is what let #5899's wording divergence exist, and it carried a
  second divergence with it — only the bulk path repaired the base repo's
  worktree bookkeeping. `tm session decommission <id>` now runs
  `git worktree prune` on the same signal the sweep always used, so
  decommissioning a tm-owned worktree interactively no longer leaves a stale
  entry in `git worktree list`.
- A 404 from the decommission endpoint is mapped to an error naming the session
  once, in `DaemonClient::decommission_managed_session`, rather than at each
  entry point. `tm session prune-idle`'s fail-closed sweep still records a
  missing session as a failed row, and no longer depends on which caller issued
  the request.
- `tm ls` and `tm sessions ls` no longer print a `-- deleted --` row for every held slot whose record has left the store (#5952). On the reporting machine those placeholders were 44 rows in a listing of ~130. They now follow the `--all` opt-in that already governs decommissioned records (#1809) and sweep-classified dead records (#4994): `--all` lists every tombstone, at its original slot number and in slot order. The interactive picker shares the same filter, so both views of `tm ls` answer the same way.
- Nothing about slot allocation changed. The registry, slot release, and `SessionManager::numbered_snapshot` are untouched, so a slot number is still never reused or reassigned (#3034); a hidden slot shows as a gap in the table's NUM column, and `--all` brings the row back. `--json` output is unchanged — a script parsing it sees every row it saw before.
- `tm` and `tm launch` in a git repository with no origin remote now start a
  session in that checkout instead of stopping at "no git origin remote found"
  / "not auto-managing this project"
  ([#6276](https://github.com/bobmatnyc/trusty-tools/issues/6276)). A two-line
  notice still says there is no remote and that no managed clone is made, and
  then the session runs. This is the end-state #6274's auto-`git init` left
  open: a first-ever `tm` run in a plain directory created the repository and
  the very next step refused it. Repositories that HAVE an origin remote are
  unchanged — a GitHub remote still gets the protected managed clone, and a
  non-GitHub remote is still refused with the live checkout untouched.
- The managed-session search-index GC and the destructive index-delete capability
  call the trusty-search daemon over its Unix socket (#6285, ADR-0032) —
  `search.indexes.list`, `search.index.status` and `search.index.delete` in place
  of `GET /indexes?details=true`, `GET /indexes/{id}/status` and
  `DELETE /indexes/{id}?delete_data=true`. Both route through
  `daemon::search_rpc`, this crate's one trusty-search client, so there is no
  second address resolver and no second HTTP client to keep in step.
- `DestructiveIndexDelete` refuses to hand out the capability when no daemon
  socket is bound, which replaces the old "no `http_addr` discovery file" refusal
  at the same point — before any request is built, and still after the #4743
  test-process refusal that is checked first.
- `tm doctor`'s `search` and `search_index_pin` checks call the trusty-search
  daemon over its Unix socket (#6285, ADR-0032) — `search.health`,
  `search.indexes.list` and `search.index.status` in place of `GET /health`,
  `GET /indexes` and `GET /indexes/{id}/status`. Both derive the socket through
  `trusty_common::daemon_socket_path("trusty-search")`, the same call the daemon
  binds, so there is no address to discover and no stale `http_addr` to disagree
  with.
- The daemon's startup banner names the trusty-search socket path instead of a
  port read off disk.
- `search_index_pin` reports a pin the daemon does not hold off the JSON-RPC
  not-found code where it used to read a 404 status; another refusal is reported
  with the daemon's own code rather than an HTTP status.
- Every trusty-memory call goes over the daemon's Unix socket (#6286, ADR-0032). `tm doctor`'s memory check, the TUI health panel, the startup banner's palace assertion, `tm memory import`, catch-up and the identity seeder all dial the derived socket instead of reading `~/.trusty-memory/http_addr`
- `tm memory import --memory-url` is `--memory-socket`, and takes a path
- The banner creates the `user` palace only on a genuine not-found, read off the JSON-RPC error code where it used to be read off a 404 status
- The `tm supervisor` process no longer serves HTTP on `127.0.0.1:7881`. It publishes each sweep's snapshot to `~/.trusty-mpm/supervisor-metrics.json`, which the daemon reads (Refs #6288).
- `SupervisorConfig.metrics_addr`, `TRUSTY_MPM_SUPERVISOR_ADDR`, and `tm supervisor --addr` are removed; a leftover value in an operator's environment is ignored.
- `DaemonLock` gains a `socket_path` field recording the socket the writing
  daemon serves; `addr` is unchanged. The field is `#[serde(default)]`, so a
  lock written by a pre-#6288 daemon still parses, with an empty `socket_path`
  meaning HTTP only. `daemon::lock::write_lock` and
  `core::daemon_identity::write_lock` / `write_lock_at` take the socket path as
  a second argument
  ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- `daemon::serve_http` waits for the shutdown signal through
  `trusty_common::shutdown_signal` and fans it out to both listeners, replacing
  a private line-for-line copy of that helper.
- `daemon::run_http` and `run_daemon` bind the RPC socket before the daemon
  publishes anything. `daemon::serve_http` takes the pre-bound socket rather than
  binding it, so a bind failure aborts before the lock file is written instead of
  leaving a record naming a daemon that never started
  ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- The bodies of the legacy session, hook, and polled-event routes moved from
  `daemon::api` into `daemon::rpc::sessions_legacy_ops`, and the axum handlers
  now delegate to them, so one route has one implementation across both
  transports. The handler signatures, paths, status codes, and response bodies
  are unchanged ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- `daemon::api::session_start_correlation` and its two hook handlers are
  `pub(crate)` rather than `pub(super)`, so the shared `ingest_hook` body can
  reach them from `daemon::rpc`.
- 1.5.7 supersedes the stranded 1.5.6 tag; ships #6343 (statusline console link)
  and #6288 slice 1 (UDS listener alongside HTTP).
- `tm statusline` no longer renders the daemon's TCP port
  ([#6304](https://github.com/bobmatnyc/trusty-tools/issues/6304)). The port was
  not actionable from a status bar and invited port-based connection attempts;
  its only other job was implicit, since it appeared only while the daemon was
  reachable. That one bit is kept as a bare `●` after the version, so the
  lead-in segment now reads `TM <ver> ●` when the daemon is up and `TM <ver>`
  when it is not.
- The statusline names the Claude Code account the session runs under, as
  `✻<email>` between the `@<gh>` and model segments (#6304). Claude Code's
  `statusLine` stdin payload carries session, cwd, model, cost, context-window
  and rate-limit fields but no account, so the email is read from
  `$CLAUDE_CONFIG_DIR/.claude.json` — the relocated personal tier every
  daemon-managed session runs under — falling back to `~/.claude.json`. A
  missing, unreadable, or malformed config, or one with no login recorded,
  omits the segment rather than rendering a placeholder. The read is bounded at
  100 ms on a detached thread, matching the sibling `gh` probe, so it can cost
  the segment but never the render.
- **The PM instruction sections and the `trusty-mpm` output style are rules as bullets, not prose.** Rationale paragraphs, incident narrative, and worked examples that only restated the rule above them are gone, and a rule stated in both surfaces is now stated once — the output style owns the voice rules, the sections own workflow and enforcement. Every rule, table, `<!-- ... -->` marker, section name, and `Skill(skill="tm-*")` pointer is preserved; no rule was replaced by a pointer to a skill (the #6320 owner ruling). `PM_INSTRUCTIONS_VERSION` is 0023 and the three composed-prompt goldens are regenerated ([#6320](https://github.com/bobmatnyc/trusty-tools/pull/6320))
- The peer bus's delivery boundary moved from the instance to the client
  ([#6462](https://github.com/bobmatnyc/trusty-tools/issues/6462), DOC-60 §7).
  Every subscriber of one instance used to share a single 64-slot broadcast
  ring, which is why #4271's fix had to refuse the publish to stay truthful: one
  client that stopped reading made `POST /api/v1/bus/publish` (and
  `mpm.bus.publish`) answer `503` for every publisher and every healthy
  co-subscriber, until that client drained, disconnected, or the instance was
  deregistered. **That instance-wide wedge is gone.** Each subscription now has
  its own 64-envelope inbox, so a stalled client falls behind alone: publishes
  are accepted, co-subscribers keep receiving, and the stalled client's own
  inbox displaces its oldest unread envelope to make room.
- `BusError::SubscriberLagged` and the `503` / `CODE_UNAVAILABLE` answer are
  retired from the publish path — a slow subscriber is no longer a publish
  failure, so there is nothing for a sender to retry. The `400` / `403` / `404`
  / `409` / `410` statuses are unchanged, and `BusError` is now
  `#[non_exhaustive]`.
- Every loss is recorded. The DOC-60 §9 durable stream now carries a second
  record shape alongside the envelope records — `{"record":"inbox_miss",…}`,
  naming the lost `message_id`, the `instance_id`, the `subscription_id` that
  lost it, and that subscription's running loss count. Readers of
  `logs/bus/*.jsonl` must tolerate it: an envelope line has no `record` key, a
  miss line always does. Nothing recorded `delivered` is lost without a matching
  miss record.
- A publish that no inbox accepted is recorded `dropped` and answered `409`,
  never `delivered`. This is reachable when a deregistration closes every
  subscription while a publisher is mid-flight.
- A subscription that attaches after its instance was deregistered ends
  immediately instead of hanging. `subscribe` resolves the instance and then
  attaches, so an attach could land after the deregistration had already closed
  every inbox present — leaving one that nothing would ever close, whose SSE
  stream stayed open on keep-alives forever. An instance's inbox set is now
  terminal once closed, under the same lock the attach takes.
- When a miss record cannot be written — the durable sink is failing, and §9's
  logger swallows that by design — the delivery's own record carries
  `losses_unrecorded: true` and an `error!` is emitted, so no line claims a
  clean delivery the stream cannot back. The field is absent in every other
  case, so an ordinary record is byte-identical to what it was.
- `GET /api/v1/bus/subscribe/{instance_id}`'s `event: lagged` frame now also
  carries `subscription_id`, which is the key to find that client's miss
  records in the durable log. Recovery is unchanged: re-read the log named in
  the frame from your last known `message_id`.
- A dropped subscription detaches itself, so a client that disconnects stops
  being fanned out to and stops holding envelopes nobody will read. A client
  that stays attached and never reads — a TCP-wedged SSE body, where the
  subscription is never dropped — costs its instance a bounded buffer plus one
  miss record and one `warn!` per subsequent publish, for as long as it stays
  attached. No publisher or co-subscriber pays for it; the operator's lever is
  deregistering the instance, and the `warn!` names which one.

### Removed

- The `trusty-bm25-daemon` entry in the bundled service manifest. #5329 moved BM25 inside trusty-memory, so `tm services` has no separate process to discover under that name.
- The `daemon::discover` module, `TrustyAddrs`, `TRUSTY_SEARCH_DEFAULT_ADDR` (the
  compiled-in `127.0.0.1:7878`), and `DaemonState::{set_trusty_addrs,
  trusty_addrs}`. trusty-search has no port to discover since ADR-0032, nothing
  read the stored addresses, and `~/.trusty-search/http_addr` is stale on every
  machine that ran the old daemon.
- `TrustyAddrs::memory`, `TRUSTY_MEMORY_DEFAULT_ADDR`, and the `~/.trusty-memory/http_addr` read in `daemon::discover`. trusty-memory has no port to discover since ADR-0032; nothing read the field, and the file it read is stale on every machine that ran the old daemon

### Security

- Control-plane session routes (`/api/v1/control/sessions/*`) now reject non-loopback callers with 403, mirroring the `/rpc` guard, and `ctl_run_session` validates all three caller-supplied spawn inputs before spawning: `claude_cmd` (only the default `claude` is accepted over HTTP), `prompt_file` (absolute, no `..`, no shell metacharacters, existing file), and `workdir` (absolute, no `..`, existing directory). The tmux backend additionally shell-quotes both interpolated fields (`claude_cmd` and the `prompt_file` path) at the sink via `shlex`, so a value with shell metacharacters typed into the pane shell is inert regardless of caller-side validation — closing a command-injection hole (Refs #6197).

### Documentation

- **Bugfix batching is now the default for same-module bug clusters.** The `tm-workflow` skill's "One Outcome, One PR" section states that several open bugs constrained to the same file, module, or tightly-coupled area go in one PR — one engineer, one worktree, one review, one CI pass — instead of one PR per issue, with each issue keeping its own regression test and `Refs #N`. `tm-delegation-patterns` adds the matching anti-pattern row: one PR per issue for same-module bugs is the anti-pattern; the fix is one batched dispatch/PR per file-cluster.
- Regenerated the `tm-capabilities` CLI reference so it carries `ls`'s `--no-prune` help text from [#6069](https://github.com/bobmatnyc/trusty-tools/pull/6069), clearing the drift-check gate that PR left red on main
- Regenerated the `tm-capabilities` bundled skill so its doctor check table
  and count carry the `base_clone` check added in #6457, clearing the
  drift-check gate that PR left red on main. (Refs #3605)
- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- **`tm-workflow`'s Worktree Discipline section now points at BASE-AGENT for the post-merge cleanup rule instead of restating it**, and drops the unconditional `git branch -D` it previously recommended — verified that plain instruction leaves every local branch undeleted on this repo, since every merge here is a squash merge ([#5768](https://github.com/bobmatnyc/trusty-tools/pull/5768))
- `session_new`'s MCP trait doc comment (`mcp/mod.rs`) no longer describes every managed session as an isolated clone; it now names the main-checkout default and the write boundary (ADR-0037, ADR-0044), matching the sibling tool description in `mcp/tools/session.rs`.
- **The resident PM prompt now states that a dispatch brief carries findings, evidence and constraints rather than the implementation mechanism**, and that a reviewer's suggested fix is relayed as a suggestion to verify rather than as an instruction — five lines in `core.md`'s "Delegating Well", added after a session where a PM prescribed mechanisms in five briefs from descriptions of code and was overruled every time by an agent that read the source ([#5792](https://github.com/bobmatnyc/trusty-tools/pull/5792))
- **An acceptance criterion must now be written so a wrong implementation fails it**, with `tm-delegation-patterns` carrying the five worked failures and a weak-vs-stronger criterion table; its "Delegation Best Practices" items on context and acceptance criteria are reworded to carry both rules, and the Structural Delegation Brief template gains a `Findings` field ([#5792](https://github.com/bobmatnyc/trusty-tools/pull/5792))
- `tm-delegation-patterns.md`'s "Long-Wait Delegation" and "PM Re-Engagement"
  sections now name `tm wait --for run|file|check --timeout <secs>` as the
  sanctioned in-turn wait for an agent's own condition, and as a bounded
  one-shot-plus-rerun alternative for CI when a brief explicitly wants the
  agent to hold its own turn. PM-side re-engagement stays the default CI
  pattern, since a CI settle notification still is not agent-visible
  (refs [#5843](https://github.com/bobmatnyc/trusty-tools/issues/5843),
  closure condition 2).
- `resolve_gh_account_env_for_registry` still pointed at
  `find_pinned_gh_account`, which #5864 renamed to `find_pinned_gh_identity`.
  The doc names the current function now (#5973).
- `PickerChoice::LaunchNew` linked `LaunchIsolation::OwnWorktree` by bare name,
  but `session_picker` never imports that type — it re-exports four other items
  from `picker_launch_new` and not this one. The link carries the full path now.
  The failing lib doc had been hiding it: rustdoc never reached the `tm` binary
  while `gh_account.rs` still failed (#5973).
- `doctor_worktrees.rs`'s `[`run_doctor`]` intra-doc link no longer breaks
  `cargo doc` for the workspace. `run_doctor` lives in the sibling module
  `daemon::doctor` and was never imported into `doctor_worktrees.rs`, so the
  bare link did not resolve; it now reads
  `[`crate::daemon::doctor::run_doctor`]`.
- The `daemon::rpc` module headers link their own items again. Each submodule
  carries a `///` doc on its `pub mod` line, which merges with the module's `//!`
  header and makes rustdoc resolve the whole doc in `daemon::rpc` — so `register`,
  `METHODS`, `DaemonError` and every `super::` path written inside a header
  rendered as dead literal text and denied `cargo doc`. Each header now ends with
  `crate::`-rooted reference definitions. `api.rs`'s links to
  `TmuxService::spawn_claude` and `PairingService::reset` are qualified the same
  way; `daemon::socket`'s private `build_router` is a code span, since docs.rs
  cannot link a private item.
- One broken intra-doc link on `daemon::doctor::run_doctor_for_manager`, which
  failed the pre-publish rustdoc gate (`scripts/check_rustdoc_links.sh`).
  `SessionManager` is referred to by its full path in the signature and never
  imported, so the doc link now says `crate::session_manager::SessionManager`.
- Regenerated the `tm-capabilities` bundled skill so its doctor check table
  and count carry the `tmux_options` check added in #6475, clearing the
  drift-check gate that PR left red on main. (Refs #6469)

## [1.4.1] — 2026-08-14

### Fixed

- `trusty-mpm-v1.4.0` was tagged but never published: its root `Cargo.toml`
  still pinned `trusty-common = "0.33"` at tag time, and the only `0.33.x`
  release on crates.io (0.33.0) lacks the `gh-cli` feature that 1.4.0's
  `get_origin_url` work depends on, so `cargo publish` failed at packaging
  before any upload occurred. `trusty-common` 0.34.0 published after the tag,
  and `main`'s requirement moved to `"0.34"` afterward too — both landed too
  late for the immutable `trusty-mpm-v1.4.0` tag to pick up. 1.4.1 carries the
  same 1.4.0 content against the now-current `trusty-common` requirement and
  is the first version of this release actually shipped to crates.io.

## [1.4.0] — 2026-08-14

This release ships a library-surface breaking change (below) as a MINOR bump.
`trusty-mpm` is a binary-only consumer surface — users install it as the `tm`
executable via `cargo install trusty-mpm` — with zero workspace dependents and
zero crates.io reverse dependencies, so the break carries no downstream
library-compatibility impact. It is recorded in
`scripts/semver-checks-crate-exclusions.tsv` and the release semver gate skips
this crate accordingly.

### Breaking

- `daemon::managed_routes::inproject::get_origin_url` returns
  `Result<Option<String>, String>` rather than `Option<String>`, so a git
  failure can no longer be mistaken for an absent remote (refs
  [#4734](https://github.com/bobmatnyc/trusty-tools/issues/4734)). A caller that
  wants the previous fail-open behaviour now spells it out with
  `.ok().flatten()`.
- `ColdStartError` gained an `OriginUnreadable` variant, so an exhaustive match
  over it needs a new arm.

### Added

- `tm-workflow` now carries a "Concurrent Sessions on One Repo" protocol: use
  `session_list` to find the other session on this repo and `session_activity` to
  read its raw tmux pane, claim work on the tracker before starting, and verify
  every `session_send` with `session_activity`. `session_send` and
  `session_proxy_message` inject characters into a pane and do not deliver a
  message — an observed long single-line send landed as a bracketed paste, the
  submit never fired, and the text sat unsent in the target's prompt buffer. The
  tools existed and were documented nowhere, so every cross-session handoff went
  through the human owner by hand.
- All three bundled output styles gain an ASD-STE-100 (Simplified Technical
  English) sentence-construction layer in `Communication — Write Plainly`: one
  idea per sentence, ~20/25-word sentence targets, active voice, one meaning
  per word, the same term for the same thing, no noun cluster over three
  words, present tense. The ~900-word approved vocabulary is named as NOT
  adopted so a future reader cannot tighten it into literal conformance. The
  stance rules it sits on top of are unchanged; the superseded "Short
  sentences, one idea each" bullet is folded into the layer rather than left
  beside it (#4574).
- The composed PM prompt now carries a "Messages Are Pointers" rule in its core
  section: a cross-session message is a pointer, and long-form content —
  findings, evidence, rationale, defect analysis — belongs in an issue or PR
  comment the message links to. A message dies with its session and is never
  indexed or searchable, so long-form content sent that way is stored where no
  third party and no later session can recover it (#5629). PM-prompt goldens
  regenerated.

### Fixed

- The mandatory pre-push credential scan now scopes its diff with three dots — `git diff origin/main...HEAD` — in both places that state the rule: the composed `WORKFLOW` instruction section and the `tm-workflow` skill's "Git Security Review". Two-dot diffs the two commits, so files DELETED from `main` since the branch point are reported as the branch's own additions
  - measured, not theoretical: one session's two-dot scan returned 19 hits that belonged to another PR's deletions, where three-dot returned zero across 36 files. The same session's earlier pre-first-push scan had been accurate only because `origin/main` still equalled its branch point — correct by luck, and on a busy `main` that luck runs out silently
  - the severity is the noise, not the wording. A credential scan that reports hits nobody owns is one reviewers learn to wave through, and a real secret arrives inside that noise
  - both statements now carry the reason inline, so the next reader does not simplify the third dot back out
- `tm` launch-new-session no longer fails on a host where tmux has never run (closes [#3886](https://github.com/bobmatnyc/trusty-tools/issues/3886))
  - the #3823 server-up guard moved into `SessionManager::resolve_session_name`, adjacent to the `list-sessions` probe it protects, so the in-project launch routes (`spawn_managed_on_main`, `reserve_inproject_worktree`) that bypass `create_with_id` inherit it
  - `tmux error:` is no longer printed twice — `names_for_serial_allocation` and `dedupe_session_name` propagate the already-typed `ManagedError` instead of re-wrapping it
- A repository git cannot read is no longer reported as one with no remote
  (refs [#4734](https://github.com/bobmatnyc/trusty-tools/issues/4734)).
  `inproject::get_origin_url` returned a bare `Option`, so "this repo has no
  `origin` remote" and "git could not answer at all" were the same value, and
  every caller read it as the former. `tm launch` and the guided default told
  the operator a checkout they could not read had no GitHub remote, and sent
  them to `tm connect` to fix a repo that is fine. It now returns
  `Result<Option<String>, String>`: a git-exec failure, or a fatal exit such as
  128 for an unreadable `.git/config`, a dangling gitdir pointer, or a
  `safe.directory` ownership refusal, is `Err`, while git-config's exit 1 — its
  "the key is not set" answer, and what both a plain non-git directory and a
  repo without an `origin` produce — stays `Ok(None)` and still falls through
  by design.
- A `.git` git cannot open no longer answers with a DIFFERENT repository's
  remote. Discovery walks past an unreadable `.git`, so a `.git` directory at
  mode 000 exited 1 with empty stderr (indistinguishable from "no origin"), and
  the same directory nested inside another repo exited 0 with the PARENT repo's
  origin — which would have the daemon provision a managed clone of the wrong
  repository. When `path/.git` exists, git's own `rev-parse --show-toplevel`
  must now name that path back.
- A managed checkout whose remote git cannot read is no longer reported as
  having no remote, which told the operator to move or remove a directory that
  may be fine.
- `validate_and_repair` no longer reports `repaired: true` for a repair that
  failed (closes [#4781](https://github.com/bobmatnyc/trusty-tools/issues/4781)).
  The flag was set unconditionally on the repair path, so a workspace whose
  repair was refused outright — `prepare_session_with_repo_url` returning the
  fatal `PrepError::Instructions` (#4752) — still reported itself repaired next
  to a populated `repair_error`. It is now derived from the re-validation:
  `true` only when the pipeline returned no error AND the post-repair report has
  no gaps.
- `tm validate --repair` prints the attempt's diagnostics again. It gated them
  on `repaired`, which under the new meaning would have silenced the error
  message on exactly the runs that produce one; it now gates on whether the
  pre-repair report had gaps, and says so when the repair did not restore the
  workspace.
- `SmMemory` and `PortfolioMemory` no longer destroy a palace's metadata when the palace exists but cannot be opened. Both `ensure_palace` implementations treated every `open_palace` failure as "the palace does not exist" and fell through to `create_palace`, which rewrites `palace.json` unconditionally — so a palace that merely could not be READ lost its `created_at` and only then re-failed against the same unreadable store. They now fall through to create only when `PalaceStore::metadata_present` definitively reports no `palace.json`, and fail closed on a probe that cannot complete (#4911).
- `tm doctor`'s `gh_account` check no longer reports "gh is not authenticated"
  for a working `GH_TOKEN` / `GITHUB_TOKEN` login. Its `gh auth status` probe
  was bounded at 250 ms — a constant inherited from the statusline's render
  path — while validating an env token takes a network round trip that measured
  281–389 ms, so every probe timed out and the timeout rendered as a definite
  negative. The doctor now probes under its own 5 s bound, and a probe that
  still does not finish reports the auth state as UNKNOWN with the reason,
  instead of claiming the account does not exist.
- The doctor reads the active login and the logged-in list from ONE
  `gh auth status` invocation (previously two, one of them `--active`), so the
  check costs a single network round trip.
- The assembled session-manager prompt told the SM to watch tcode for
  `__HARNESS_EVENT__`; tcode emits `__OMPM_EVENT__`. The harness-understanding
  sections now carry the emitted marker, and the prompt tests assert against
  `trusty_agents_common::events::EVENT_LINE_PREFIX` instead of a literal — they
  previously passed by agreeing with the wrong instruction text
  ([#5129](https://github.com/bobmatnyc/trusty-tools/issues/5129)).
- `tm-ticketing` skill: removed the `Provenance` label family, which listed
  `trusty-mpm` as a harness default and let the ticketing agent apply that label
  to any issue it filed regardless of which crate the defect lived in. The skill
  now states three label families and rules out an origin-based axis under any
  name — provenance, umbrella, or dogfooding — matching the agent asset fixed in
  #5691.
- The launchd probe no longer reads an unreadable `~/Library/LaunchAgents` as
  "no launchd unit is registered"
  (closes [#5623](https://github.com/bobmatnyc/trusty-tools/issues/5623)).
  `daemon_launchd_label_in` mapped every `read_dir` error to the same answer as
  an empty directory, and `tm start`, `tm restart` and the MCP stdio bridge all
  read that answer as permission to spawn — creating the unsupervised orphan
  daemon of #2486/#4230. It now answers in three states, and both spawn sites
  and `tm doctor`'s `daemon_orphan` check treat "could not determine" as
  "launchd may own this" rather than as absence. A home with no `LaunchAgents`
  directory still resolves to "no unit registered" (ADR-0045 §3).
- The `tm-issues-prune` skill no longer presents a truncated backlog as the
  whole one. Its scan and prioritize passes hardcoded
  `gh issue list --limit 500` against a repo with 745 open issues; they now
  paginate to exhaustion and report retrieved-vs-total, so a short read
  announces itself.
- `tm-workflow` and `tm-delegation-patterns` now name harness-managed
  `isolation: "worktree"` as the only sanctioned isolation mechanism for a
  file-mutating subagent dispatch, and state that the PM serializes when
  isolation is unavailable rather than hand-rolling `git worktree add` into a
  prompt. The old "name the exact worktree path" wording led PMs into a
  hand-rolled worktree the `tm hook --pm-guard` guard cannot see, which then
  denied the next dispatch for a collision that did not exist (#5649).
- The shared-worktree deny message now offers the serialize fallback alongside
  `isolation: "worktree"`, and says the guard reads the declared parameter and
  never the prompt (#5649).
- `tm hook --pm-guard` now denies an unisolated `documentation`,
  `version-control`, `qa`, `web-qa`, or `api-qa` dispatch that would share a
  running file-mutating agent's working tree. The classifier recognised only
  engineer-tier agents, so those five wrote into an engineer's tree on one git
  HEAD with no deny at any step (#5650).
- `code-critic` still classifies as non-mutating. It declares the same
  `role: qa` and `extends: base-qa` as the three QA writers but only reviews, so
  the QA agents are matched by name rather than by role — a role-based widen
  would have denied a review dispatched alongside the engineer it reviews
  (#5650).
- `tm-workflow` now lists which agents count as file-mutating, replacing wording
  that said QA agents get their own worktree while the guard classified `qa` as
  non-mutating (#5650).

### Changed

- `core::gh_account`, `core::gh_account_enforce`,
  `core::session_launch::workstream_label`, and
  `session_manager::worktree_reclaim` invoke `gh` through
  `trusty_common::gh::GhCommand`
  ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)). Timeout
  bounds, `GH_CONFIG_DIR` scoping, the `GH_TOKEN` env stripping, and the
  `current_dir`-never-`-C` rule (#2919) are all unchanged — `worktree_reclaim`
  keeps its own kill-on-expiry runner and takes the unspawned command from the
  entry point.

## [1.3.6] — 2026-08-12

### Added

- Sessions now carry their two per-project MCP pins as environment variables the
  spawn exports: `TRUSTY_MEMORY_PALACE` (the project's palace slug) and
  `TRUSTY_INDEX` (the confirmed trusty-search index id). A single shared
  user-scope declaration cannot carry a per-project argument, so these replace
  the `env` block and the `serve --index <id>` argument the deleted injectors
  wrote. Both stay gated on the same `[mcp] trusty_memory` / `[mcp] trusty_search`
  manifest toggles, and each is omitted rather than exported empty when the
  toggle is off or the value cannot be derived — so a session keeps the right
  palace (#1605) and the right index (#1373) without a workspace `.mcp.json`
  (#4181, ADR-0042).
- `BASE-AGENT.md` now carries the agent-facing half of the shared-working-tree
  rule: a file-mutating agent works in its own worktree and never
  `git checkout`/`git switch` in a directory it was handed, because a
  concurrently-dispatched sibling shares that git HEAD. It names
  `tm hook --pm-guard` as the enforcement so the deny message and the prose point
  at each other (#4480).
- `tm hook --pm-guard` now denies a second concurrent file-mutating `Agent`/`Task`
  dispatch into a working directory that already has one, unless the dispatch
  declares `isolation: "worktree"` (or `"remote"`). Two subagents sharing the PM's
  directory race over one git HEAD, and git does not refuse the collision — a
  `git checkout -b` only refuses when a tracked file differs between both branches
  AND has an uncommitted change, so untracked files transfer onto the wrong branch
  silently. The guard fires only for bundled engineer-tier agents and fails open
  everywhere else: a read-only dispatch, an unknown agent name, an unresolvable
  working directory, and an unreachable daemon all allow the dispatch (#4480).
  - Delegation records now carry the `isolation` mode a dispatch declared, and the
    daemon serves `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` so
    the guard can learn which agents are already writing in a directory rather than
    guess from a timer.
- the interactive session picker (bare `tm`, `tm ls`) accepts `n` to launch a new session and `n <name>` to name it (closes [#4965](https://github.com/bobmatnyc/trusty-tools/issues/4965))
  - `n` is slot-independent: the `[N] launch new session` number shifts as sessions come and go, so there was nothing to memorize — that entry stays, and `n` is additive
  - the token must be exactly `n`/`N`, and a name requires a whitespace separator, so `new`, `no`, `nn`, `n1` and `nauth` are all unrecognised — no n-initial typo can spawn a session
  - `n <name>` launches as `tm-<name>-NN`, with the name kebab-cased in the CLI before it is sent (`n My Auth Fix!` → `tm-my-auth-fix-01`); sanitizing here is what keeps `n feature/auth` and `n hotfix/auth` distinct, since the daemon's own hint sanitization takes the path basename and would collapse both to `auth`
  - a name with no usable characters (`n !!!`) is refused with a message rather than spawning under the daemon's shared `local` fallback leaf
- Session launch now ensures the `in-progress` and `blocked` issue-lifecycle
  labels exist, alongside the existing `ws/<session>` label, so
  `tm-ticketing`'s Lifecycle rule works on a repo that has never seen them.
  Fire-and-forget and non-fatal, exactly like the `ws/` ensure — a `gh`
  failure on any label never blocks a launch or suppresses the other labels.
  Unlike `ws/`, the two lifecycle labels are created WITHOUT `--force`, since
  a project may already own and have styled them.
- The resident PM `workflow` instruction section now points at that Lifecycle
  section when work is dispatched against an issue; previously the rule was
  only reachable if the PM happened to load the trigger-loaded skill.
- `tm hook --pm-guard` denies whole-tree-destructive git commands — `git checkout -- <pathspec>` / `checkout .` / `checkout -f`, `git restore` in its working-tree forms, `git reset --hard|--merge|--keep`, a forced `git clean`, and `git switch --discard-changes` — when the directory they would act on is a project's main checkout rather than a worktree. It binds dispatched subagents, not only the PM, and builds the enforcement ADR-0037 deferred. Read-only git, `checkout -b`, a plain `git reset`, `git restore --staged`, a dry-run `clean`, and everything under `.claude/worktrees/**` stay allowed.
- `tm doctor` gained a `stray_mcp_json` check for `.mcp.json` files sitting
  ABOVE a workspace or in a temp root. Claude Code discovers `.mcp.json` by
  walking UP from a session's cwd, so one written above real projects supplies
  the MCP servers of every session started beneath it — agent scratchpads under
  `/tmp` included — with nothing in the project to point at. The scan is
  bounded to the workspace's strict ancestors up to the home directory, plus
  `$TMPDIR` and `/tmp`; never the filesystem root, never a recursive descent
  (PR #5371).
- A provenance ledger at `~/.trusty-mpm/mcp-json-provenance.json` records every
  `.mcp.json` tm writes, with a checksum of the bytes written. It is what lets
  a repair tell tm's own output from a file the operator wrote by hand — a
  distinction the file's contents cannot supply, since an operator may register
  the same `trusty-*` servers themselves (PR #5371).
- `tm doctor --fix` now quarantines stray `.mcp.json` files the ledger proves
  tm wrote and nobody has edited since, and `tm doctor --quarantine-mcp <PATH>`
  quarantines one the operator names explicitly. Both are DRY RUN until
  `--yes`. Neither deletes: the file is RENAMED to
  `.mcp.json.quarantined-<epoch>` beside itself, which is what stops the
  upward walk finding it while keeping every byte recoverable. Unattributed
  files, files edited after tm wrote them, symlinks, an unreadable ledger, and
  the workspace's own `.mcp.json` are each REFUSED with the reason shown
  (PR #5371).
- `session_context_catchup` takes an optional `tmux_window` and resolves a
  snapshot by the caller's tmux window id when `session_id` owns none. A Claude
  Code relaunch mints a new harness session id inside the same window, which
  left `resolved_snapshot` null and pushed `/tm-session-resume` back to a human
  guessing which snapshot was theirs. The response also carries `resolved_via`
  (`session_id` / `tmux_window` / null) so a window match is never read as an
  exact one.
- `tm-workflow`'s "Worktree Discipline" section now tells sessions to keep the main checkout fresh: `git fetch` then `git pull --ff-only` at session start and after a PR this session merged lands on `origin/main`. Worktrees branch off `origin/main` and stay current, but nothing refreshes the main checkout, so it drifts and inspection reads start answering from old code
  - fast-forward only, so the refresh cannot create a merge commit or rewrite history, and a dirty tree only blocks it when the incoming commits touch the same files
  - when `--ff-only` is blocked by another session's uncommitted changes, the rule is preserve rather than discard — commit the orphaned work to a throwaway branch, then switch back and fast-forward. Committing reaches the same clean tree as discarding, costs the same number of steps, and cannot lose anything
  - `git stash` stays banned (repo-level, shared across every worktree), as do `git checkout --`, `restore`, `reset --hard`, and `clean` — the main-checkout guard blocks those and hunting for an unblocked equivalent is routing around a safety control. A file showing ` M` in `git status --porcelain` was never staged, so nothing recovers it once discarded
  - stated as inspection hygiene: the main checkout stays read-only for work, all edits still happen in a worktree, and `git pull` is already on the PM's allowlist so this needs no new authority
- The `tm ls` picker's delete accepts a name pattern: `d tm-test-*` deletes
  every matching session in one action instead of one `d<N>` per session. The
  pattern is a glob (`*`, `?`, `[…]`, case-insensitive) matched against the tmux
  session name, and `d <glob> --dry-run` previews without deleting. Deletions
  route through the same managed→local path `d<N>` and `tm session delete`
  already use. Four guards bound the blast radius: only stopped/errored sessions
  are ever deleted in bulk (a running match is listed as kept, and force is
  never available in bulk); the session you are running inside is excluded by
  tmux name even if its record reads stopped (best-effort — the driver warns
  when that probe fails rather than implying a guard it did not apply); a
  session with no tmux name matches no pattern at all, `*` included; and
  confirmation is the number of `DELETE` rows typed back, not `y`, after the
  full match set is printed. The prompt deliberately does not print that number
  — printing it would let the batch be confirmed without reading a row. A
  pattern matching nothing says so and exits without deleting, and a per-session
  failure mid-batch is reported and tallied instead of aborting the run.
  Documented for agents in the bundled `tm-cli-operations` skill.
- `tm ls -a` / `tm ls --attached` lists only the sessions a tmux client is attached to right now
  - "attached" is tmux's own `#{session_attached}`, surfaced by the daemon as the per-session `attached` flag — a session that is live and reported `active` with nobody connected to it is EXCLUDED, which is the whole point of the flag
  - forces the static table for the same reason `--all` does: a session you already have a client on is the one target the picker cannot usefully connect you to
  - matching nothing prints `no attached sessions` (or `no attached sessions for <slug>` when scoped) and exits 0 — an empty result reads as a real answer, not a failure or an empty table
  - no effect on `--json` (the raw daemon response stays complete, matching `--all` and the filter terms) or on `--projects`; `tm sessions ls` and `tm f` are unchanged
  - `-a` NARROWS here, inverting the `ls -a` / `docker ps -a` convention where it widens; `--all` remains long-only and the two flags are independent

### Fixed

- The `tm-ticketing` skill's confidence-state marker (`Observed` /
  `Reproduced` / `Inferred` / `Speculative`) is now scoped to defect (`bug`)
  issue bodies. It used to be required on every filed issue, which produced
  lines like `Confidence: Observed … — accepted feature work` on feature
  tickets — a restatement of the type label that carries no information,
  since a feature ticket has no "reproduced vs. suspected" axis to report.
  Bug tickets are unaffected: the marker still distinguishes a reproduced
  defect from a suspected one there, which changes what a reader does next.
- `standalone::trust_seed::preseed_managed_trust` now holds
  `core::claude_json_guard::lock()` across its whole read → mutate → write
  cycle. It was the third `.claude.json` seeder with the unguarded cycle #4072
  fixed for the other two, and the daemon calls it once per session it
  provisions: two overlapping cycles silently dropped one session's whole
  `projects.<workspace>` entry, so the operator met the trust dialog the seed
  exists to dismiss while both writes reported success. A concurrent seed of
  320 distinct workspaces lost 278 of them before the fix and none after
  (#4076). `mcp_config::seed_builtin_servers` already held the guard against
  the same `<claude_config_dir>/.claude.json`, so leaving this seeder out also
  left those two racing each other.
- `core::claude_json_guard`'s module doc no longer claims that a cross-process
  race "can only ever lose an update, never leave a torn file behind". That
  was false until `trusty-common`'s `write_json_atomic` stopped sharing one
  staging filename (#4077); it is true now, and the doc says which change
  makes it so.
- A relocated `tm launch` / `tm connect` now carries `CLAUDE_CODE_OAUTH_TOKEN`
  when one is stored. On macOS the Keychain entry Claude Code reads is keyed by
  a hash of `CLAUDE_CONFIG_DIR`, so relocating without the token produces a
  login that succeeds and is then immediately not-logged-in. When no token
  resolves, the commands print a one-line notice naming
  `claude setup-token | tm auth set-token` rather than leaving the operator to
  diagnose the loop (#2246, #4181).
- The shared-worktree dispatch deny now offers only the remedy that always
  works — re-dispatch with `isolation: "worktree"`. It no longer suggests waiting
  for the running agent: a crashed subagent that never emits `SubagentStop` holds
  its delegation for the full six-hour staleness window, so waiting could mean
  waiting forever (#4480).
- **A corrupt `sessions.json` is now detected, disclosed, and repairable** (closes [#5007](https://github.com/bobmatnyc/trusty-tools/issues/5007)): on 2026-08-06 the managed-session store was a complete JSON document followed by 1111 stale bytes from a longer earlier version of itself. `SessionStore::upsert` reloads before every save, so a file the daemon could not read was a file it could never rewrite — every mutation failed for an unknown period while `tm ls` kept printing a healthy fleet from the in-memory fallback, and the only fix was a human hand-truncating JSON.
  - **Detect:** a parse failure is now `StoreError::Corrupt`, whose message names the file, the byte offset where the valid document ends, how many trailing bytes follow, and the repair command — instead of the bare `trailing characters at line 3755 column 2`. A new `session_store` check in `tm doctor` reports the same thing before anyone stumbles into it.
  - **Recover:** `tm repair session-store` backs the file up to `sessions.json.corrupt-backup-<YYYYMMDD-HHMMSS>` and truncates it to the longest complete JSON document at its head. It refuses when the discarded bytes name a session id the recovered document does not contain (`--force` overrides, `--dry-run` shows the cut without writing).
  - **Stop the silent mask:** the last-known-in-memory fallback stays — a transient read error must not report an empty fleet — but it is no longer silent. Corruption logs at ERROR, and `GET /api/v1/sessions/managed` carries a `store_health` field (omitted entirely when healthy) that every `tm ls`, `tm ls --json`, and picker invocation turns into a stderr warning.
  - **Likely cause, fixed:** `save()` staged through one fixed `sessions.json.tmp` shared by every writer in every process. The rename is atomic but the staging write is not exclusive, so two concurrent savers produce a file as long as the longer document and as valid as the shorter one — the exact observed shape. Staging is now per-store-instance (`sessions.json.tmp.<pid>.<nonce>`).
  - **One definition of "healthy":** the store, `tm doctor`, and `tm repair session-store` all validate through a single entry point against the type the daemon actually deserializes. They previously used two different schemas, so `tm doctor` reported `Ok` with a record count for files that blocked every write, and a file could be simultaneously "corrupt — run `tm repair session-store`" and "parses cleanly; nothing to repair". The diagnostic now names that command only when truncating would really leave a loadable store, and no longer reports trailing junk that is not there.
  - **Related data-loss guard:** `read_file` answered every read error with an empty store, so a permissions failure or an EIO would have made the next `save()` write that emptiness over the whole fleet — and it emptied the orphan-GC's fail-closed `known_tmux_names` set, making every live tmux session look untracked and reapable. Only a genuinely absent file now means "fresh store".
- Register the base checkout behind a worktree with `trusty-search`, vector lane
  on ([#5069](https://github.com/bobmatnyc/trusty-tools/issues/5069)). Worktree
  indexes are BM25+KG only since #5060 and nothing ever created the base-facet
  index that owns the embedding lane, so every worktree session got lexical and
  graph search and never semantic. Worktree creation and session launch now both
  ensure it, on a detached thread, and report an unconfirmed registration as a
  warning instead of leaving the absence invisible. Routing a worktree's semantic
  query onto that index remains open.
- Session launch writes the UNPINNED `trusty-search` MCP stub when index
  registration was not confirmed, instead of pinning `serve --index <id>` to an
  index the daemon never created
  ([#5091](https://github.com/bobmatnyc/trusty-tools/issues/5091)). Previously
  every `search`/`grep` in such a session answered `404 unknown index` for the
  session's whole life while the `search` health check stayed green. The
  `search_index_pin` doctor check reports the unpinned stub as a warning naming
  the relaunch fix, and still fails on a pin that has since gone stale.
- Two `Agent`/`Task` dispatches issued in one PM turn could both be admitted into
  the same working directory. The `#4480` guard asked the daemon who was already
  writing there and acted on the answer, with nothing claiming the directory in
  between, so both could ask before either was recorded and both see an empty
  set. `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` now answers
  and claims in one critical section, so the second of two simultaneous
  dispatches finds the first and is denied. The claim is the delegation record
  the daemon's own `PreToolUse` hook would have written a moment later — same
  correlation key, same liveness, same staleness sweep — so nothing new has to be
  released. Every fail-open branch is unchanged: an unreachable daemon, a
  malformed answer, an unresolvable working directory, an unknown agent, and a
  read-only or isolated dispatch all still allow the call and claim nothing
  (#5324).
- The claim POST forwards only `subagent_type`, `isolation` and `description` —
  the fields the daemon actually reads — instead of the whole dispatch prompt.
  The record it writes is unchanged, and the request body can no longer grow
  past a size limit and fail the guard open on the largest dispatches (#5324).
- `tm hook --pm-guard` now warns on stderr when the daemon reports no live
  writers but claims nothing, which only happens when a running daemon predates
  the `tm` on PATH and does not recognise the agent being dispatched. The
  dispatch is still allowed — this path fails open deliberately — but the guard
  no longer goes silently unenforced (#5324).
- The Non-Overridable Rules section's `trusty-search` guidance now matches the
  pinned-first, three-tier `index_id` resolution order trusty-search's own MCP
  descriptors have documented since #5213 (explicit `index_id` wins, then the
  session pin, then fan-out only when unpinned), and points agents at
  `list_indexes` before guessing an explicit id instead of only warning that a
  guess can 404 (#5337).
- The declared version moves to an unpublished number so
  `scripts/check-version-parity.sh` has nothing to compare against, unblocking
  the red `main` parity check (#5344).
- The stray-`.mcp.json` quarantine compared paths lexically, so a caller
  holding a symlinked spelling of the workspace slipped past the guard that
  protects the project's own `.mcp.json` and the file was renamed away. Path
  comparisons now canonicalize both sides through one shared helper, which also
  fixes the scan's home ceiling — a symlinked home spelling never matched, and
  the walk ran past it to the depth cap (PR #5371).
- The sweep classified provenance during the scan and renamed later, so a file
  edited in that window was quarantined on a stale verdict, discarding the
  edit. The checksum is now re-read immediately before the rename and a file
  that changed is refused (PR #5371).
- The provenance ledger grew one entry per distinct `.mcp.json` path and never
  shed one. Above 512 entries it now drops records whose file no longer exists
  (PR #5371).
- `session_context_catchup` returned every paused session's `source_file` and
  `tmux_window` in `sessions[]` to any caller, regardless of `session_id`. A
  caller could read another session's window out of the response, hand it back
  as its own, and resolve that session's snapshot — reconstructing by hand the
  cross-session resume #5272 removed. A caller that does not own a session now
  gets `format`, `paused_at`, `summary` and `owned: false`; `source_file`,
  `tmux_window`, `in_progress`, `next_steps` and `git_context` are withheld.
  The `tm session catchup` CLI digest is unchanged.
- `core::spawn_disclaim::disclaimed_output` retries a spawn that fails with
  `ETXTBSY` ("Text file busy") up to three times with exponential back-off,
  instead of surfacing it as a hard error. `ETXTBSY` is raised while any
  process holds a writable fd to the target inode, so a sibling thread that
  forks between this process's write and close of a freshly created
  executable can make an otherwise valid exec fail — the race that made the
  `tmux` fake-binary tests flaky in CI. A permanently busy target still fails
  after the third attempt (#5391, epic #3451).
- Injected agent docs (`tm-tool-usage-guide.md`, `tm-circuit-breaker.md` CB#7, and the non-overridable "Trusty Tool Priority" rules) described `search_health` as a bare liveness check and never mentioned that a successful call no longer means the daemon is up, once PR #5534 lands. Each site now says to branch on the response's `healthy` field, not on the call succeeding, so an agent doesn't read a dead daemon as a false all-clear
- Regenerated the three committed PM-prompt golden fixtures
  (`pm-prompt-bundled-fallback.md`, `pm-prompt-claude-md-override.md`,
  `pm-prompt-roster-absent.md`) that went stale when #5536 updated the
  `search_health` liveness doc text without updating the snapshots, which left
  `golden_bundled_fallback_prompt`, `golden_claude_md_override_prompt`, and
  `golden_roster_absent_assembly_prompt` failing on every run.
- Managed sessions (`tm launch`, `tm connect`, daemon spawns, `tm register/load/run`) no longer resolve the user-global `~/.claude.json` and `~/.claude/settings.json` through a path the managed session layout rewrites onto the workspace. `prepare_session` now takes the home directory as a parameter rather than deriving it from `FrameworkPaths`, whose `.claude/` paths a managed session deliberately relocates — so a session cannot drop an untracked `.claude.json` into the operator's repo, and the global `trusty-memory` hook cleanup targets the real settings file instead of silently no-opping on one that does not exist. Production behaviour is unchanged under every framework root, including an overridden `--root`/`TRUSTY_MPM_ROOT` (#5544).
- The `tm` binary's test suite no longer repoints `$HOME` or `$CLAUDE_CONFIG_DIR` process-wide — a write that could straddle a parallel test's agent-roster scan and report a truncated roster. A mechanical guard fails the build if either comes back (#5544).
- A delegation roster that lost agents to a failed read now says so. An agent
  file that exists but cannot be read used to be dropped by a bare `continue`
  with no log, and a tier that failed to enumerate returned an empty list
  indistinguishable from an absent tier — so a PM was handed a short roster and
  read it as the complete set. Both losses are now recorded in a `RosterScan`,
  logged at `error!`, and rendered as a `ROSTER INCOMPLETE` banner at the top of
  the composed `## Delegation Authority` section naming every unreadable path.
  A roster that is empty *because* reads failed no longer degrades silently to
  the bundled asset. Composition stays fail-open — a bad directory must not
  block a launch — but it can no longer claim a roster it does not have
  (#5544).
- Every surface that publishes the roster COUNT now says when the count is a
  floor. `tm session start` prints `at least N agents in delegation authority —
  ROSTER INCOMPLETE: …` and names each unreadable path on stderr; `tm doctor`'s
  `agents` check downgrades `Ok` to `Warn` and reports `at least N delegatable`
  with the lost paths. Both took their number from the non-reporting
  `resolve_roster`, so a failed read lowered it silently — including in
  `tm doctor`, which is the tool the new banner tells the operator to run
  (#5544).
- `session_context_catchup` no longer returns an unbounded response. On this
  repo `full: true` returned 112,096 characters — past what the harness could
  hand back to the calling model, so it spilled the body to a file and the
  session resuming from it had to read that instead. Measurement put `sessions`
  at 94.7% of the payload with a 5,961-byte median record, so the count is what
  grows, not any one field: the digest is now paged by whole records to a
  48,000-byte budget. `sessions_offset` (new, optional, defaults to 0) selects
  the page and the response's `sessions_next_offset` names the next one, so
  `full: true` still delivers every snapshot in history — one readable page per
  call. Pages are ordered with the caller's own sessions first, then newest
  first, so page 0 always carries the entry a resume reads. Nothing is dropped
  silently: whenever anything is withheld the response carries
  `truncated: true`, the `sessions_total` / `recent_commits_total` /
  `recent_memory_total` counts, and a `truncation_notice` naming the counts and
  the exact `sessions_offset` that retrieves the rest. The one page that can
  exceed the budget — a single record larger than a whole page, which ships
  intact rather than being cut mid-field — reports `over_budget: true` and
  `page_bytes`, since nothing was withheld there and `truncated` would
  otherwise read as healthy. The offset is positional into a list rebuilt from
  disk on each call, so a snapshot paused mid-walk can make a later page repeat
  a record; that is stated in the tool schema and in the notice rather than
  left to be discovered. A digest that already fits comes back byte-identical
  (#5557).

### Changed

- The `Communication — Write Plainly` section of every bundled output style, and
  the agent-facing mirror in `BASE-AGENT.md`, now state one rule where three
  overlapped: an opener that announces a fact's significance instead of stating
  the fact is banned however it is worded. The separate "no throat-clearing
  openers" bullet and the "delete the framing opener" template block are folded
  into it, and their strings survive as illustrations alongside the owner's new
  instance, "Worth naming what just happened:". Net 6 lines shorter.
- The four remaining read-only tmux probes (`tmux_attach::current_tmux_session_name`,
  `statusline::branch::tmux_session_name`, `guided_inplace::read_tmux_env_managed_session_id`,
  `core::process::tmux_pane_pid`) now route through `core::tmux`'s resolved
  binary + TCC-disclaimed spawn primitive instead of a bare, unresolved
  `Command::new("tmux")`, via two new `core::tmux` helpers
  (`display_message_argv`/`show_environment_argv` +
  `run_tmux_argv_with_bin`/`run_tmux_argv`) added alongside the existing typed
  `TmuxCommand` path. Targeting semantics are unchanged at every site.
- `tm launch` and `tm connect` now point the session they spawn at the tm-owned
  `CLAUDE_CONFIG_DIR` (`~/.trusty-tools/trusty-mpm/claude-config`) and carry
  `--setting-sources user,project,local`, matching the daemon-managed path.
  Under ADR-0042 the MCP declaration lives once in the `user` tier of that
  directory's `.claude.json`, and a session that neither relocates nor loads
  `user` reads no MCP servers at all. #1269's isolation guarantee is unchanged:
  the `user` tier now resolves to the tm-owned config home rather than the
  operator's `~/.claude`, so the operator's global settings and hooks stay out
  by relocation instead of by exclusion. When the home cannot be resolved,
  both commands fall back to the previous `--setting-sources project,local`
  with no relocation (#4181).
- Both commands seed workspace trust into `<CLAUDE_CONFIG_DIR>/.claude.json`
  instead of `~/.claude.json` — the file a relocated session actually reads.
  The four framework MCP builtins enter `enabledMcpjsonServers` only when that
  run's injector reported an actual write, carried through from the same run's
  `PrepReport` (#3950's contract, unchanged by the move) (#4181).
- The four framework MCP servers (`trusty-memory`, `trusty-mpm`,
  `trusty-review`, `trusty-search`) are now declared once in the user-scope
  `mcpServers` map of the tm-owned `.claude.json`, seeded on every launch by
  `mcp_config::seed_builtin_servers`. Seeding is insert-if-absent: an entry the
  operator registered under one of those names via `tm mcp add` is never
  overwritten, reordered, or removed. This replaces the write to
  `<CLAUDE_CONFIG_DIR>/.mcp.json`, which no session ever read — Claude Code
  discovers a `.mcp.json` by walking up from the session's cwd, and cwd is
  always the repo (#4181, ADR-0042).
- Seeding never quarantines. A malformed `.claude.json`, or one whose
  `mcpServers` is not an object, makes it warn and return without renaming or
  writing anything; a failure of any kind is absorbed so the agent and skill
  deploy that follows still runs. The per-launch injectors and the
  `enabledMcpjsonServers` approval are untouched here and are deleted together
  in the next PR of the ADR-0042 sequence.
- `tm doctor`'s `binary_provenance` check splits a ledger/binary version disagreement by direction instead of always reporting `Fail` ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
  - running binary NEWER than cargo's record at the same path is now `Unknown` — the prebuilt installer placed it and keeps no cargo metadata, so provenance is unverifiable rather than broken
  - running binary OLDER than cargo's record stays `Fail`, unchanged: an older binary on top of a newer install is the [#4033](https://github.com/bobmatnyc/trusty-tools/issues/4033) defect
  - versions that cannot be ordered as semver stay `Fail`
  - `Warn` remains the correct terminal verdict for a `cargo install --path` install per [ADR-0043](../../docs/adr/0043-cargo-bin-policy.md)
  - the `binary_provenance` description in the bundled `tm-capabilities` skill now states the direction split instead of the retired "any version disagreement fails"
- `tm-workflow`'s "Worktree Discipline" section now carries the rest of the checkout rules that root `CLAUDE.md` used to state, so every project deploying the skill gets them rather than only this repo
  - the main checkout's forbidden-operation list is now explicit: any edit, any build or test run, any destructive git op (`git reset --hard`, `git checkout .`, `git stash`, `git restore .`), any file-mutating command (`sed`/`awk`/`patch`), and anything else that mutates the working tree, index, or build output
  - a worktree is a writer and the branch is the workstream — the branch is the durable unit, a worktree is ephemeral and recreatable, and losing one loses nothing the branch does not still hold
  - one branch and worktree per independently reviewable PR outcome, not per ticket, refactor step, or experiment
  - the stash-first escape hatch is labelled a narrow exception, not license for routine edits from the main checkout, and project-specific worktree hazards are directed to the project's own reference docs
- `docs/reference/worktree-discipline.md` gains this repo's Cargo/macOS specifics — installing a freshly built binary from the worktree, the `cargo install` cdhash-cache rationale with a cross-link to the release-workflow hazard, and the stash-first fallback for a post-merge install from the main checkout
- `BASE-AGENT.md` gains a `Never Narrate a Wait` section, placed directly before `Git Workflow` so an agent reads it before it starts work ([#5019](https://github.com/bobmatnyc/trusty-tools/pull/5019))
  - a subagent's turn ends the moment it stops emitting tool calls, and nothing re-invokes it afterward — `CLAUDE_SUBAGENT_BG_SHELL_MAX_MS` lets a subagent-owned background shell outlive the turn, but no completion event reconnects the two. Ending a turn with "I'll wait for the pull", "will resume when the monitor reports completion", or "monitoring in the background" strands the task until a human notices; observed five times in one session
  - to await a long operation the agent stays in the turn: start it with `run_in_background`, then poll an until-loop against its output file. The loop is backgrounded too, because foreground `sleep` is blocked in this harness — a prior revision's bare "poll" advice was itself producing the parking it meant to prevent
  - reporting unfinished state and stopping ("still pending: head SHA abc1234, 10 checks unsettled") is now stated as a correct, complete outcome; the failure is stopping while implying you will continue
  - never re-issue a long-running command because a foreground shell call returned early at the ~120s cap and auto-backgrounded — check whether the original is still running first
  - the now-redundant "never spawn a background monitor as a wake mechanism" bullet under `Your own gates DO block, in the foreground` collapses to a pointer at the new section
- A decommissioned or deleted session releases its `tm ls` `NUM` after 24 hours
  instead of 7 days (`TERMINAL_RECORD_RETENTION_DAYS`, 7 → 1).
- The retention guard that spared a record whose workspace directory still
  existed now spares only a record whose workspace is a session WORKTREE —
  recognised by the `.worktrees/<leaf>` shape or a `.trusty-mpm-worktree`
  ownership sentinel. A session launched on a project's main checkout records
  that checkout as its workspace, and the old guard read it as a worktree to
  protect, so such records were never evicted at any window. Records from
  `tm launch --worktree` are protected exactly as before, at any age.
- `tmux.alternate_screen` now defaults to `false`, so a tm-managed session has
  working scrollback out of the box ([#5364](https://github.com/bobmatnyc/trusty-tools/issues/5364)).
  [#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151) added the knob
  to fix missing scrollback but shipped it defaulting to `true` — tmux's factory
  value and the pre-fix behaviour — so the original bug persisted for every
  operator who never found the knob. A full-screen TUI draws into tmux's
  alternate screen, and tmux does not append that output to the pane's
  scrollback at all; `history-limit` is irrelevant in that state.
- **Tradeoff, accepted deliberately:** `alternate-screen` is server-global and
  every trusty-* managed session shares one tmux server, so `vim`, `less`,
  `htop` and `man` no longer restore the terminal's prior screen on exit
  server-wide — each leaves its final frame in the scrollback. Set
  `tmux.alternate_screen: true` in `~/.trusty-tools/trusty-mpm/config.yaml` to
  restore the old behaviour.
- The `Communication — Write Plainly` section of every bundled output style, and
  the agent-facing mirror in `BASE-AGENT.md`, now ban borrowed-metaphor
  jargon — "load-bearing" and its category — and require the mechanism be
  stated instead (#5372).
- `tm-workflow` gains a `Test Scope Widens by Stage` section, placed after the Sprint/Harden doctrine it refines ([#5377](https://github.com/bobmatnyc/trusty-tools/pull/5377))
  - unit tests run on the new or changed code only while developing, on the full test files that changed when merging, and on the full corpus only when publishing
  - merging widens to whole files, never to the whole repository — including cases you did not edit and any normally-skipped test in those files. A change to a public interface or to shared test infrastructure makes a dependent package's test files count as changed even though the diff never opened them
  - stated stack-agnostic, since `tm-workflow` governs every project: the section names no build-tool command
  - stage and rigour are named as separate axes that both bind — the stage decides how wide a gate runs, the project's risk labels and test ladder decide how hard it is applied and which gates run at all
  - a project may leave a full-corpus CI job off its required pre-merge contexts, since that job is a publish gate. A *failing* check still blocks the merge at every stage; only *pending* is tolerated
  - the hard line is restated in place: choosing the narrower scope is a blast-radius claim you must be able to prove, never licence to make a red gate green by deleting a test, marking it skipped, gating it out of the build, or excluding it from the run
- The ban on "honest" moved out of the composed system prompt's
  `pm-instruction-package.json` block and into the three output styles'
  `Communication — Write Plainly` section and both `BASE-AGENT.md` copies — the
  channel a manual `claude` launch keeps. It now names every position the word
  takes (adjective, adverb, heading modifier, parenthetical), so it reaches
  `<noun>, stated honestly:` as a heading, and it absorbs `BASE-AGENT.md`'s
  separate never-announce-your-register bullet. One copy, resident once (#5420).
- `core::spawn_disclaim::disclaimed_output` retries `ETXTBSY` through `trusty_common::spawn_retry::retry_on_etxtbsy` instead of its own copy; the contract tests #5391 added move to trusty-common with the code, and gain async twins and a re-invocation table pinning that only `ETXTBSY` retries ([#5446](https://github.com/bobmatnyc/trusty-tools/issues/5446))
- `code-critic` now posts its verdict (APPROVE/WARN/BLOCK) as a COMMENT-type
  GitHub review (`gh pr review --comment`) instead of a plain PR comment, so
  it groups under the PR's Reviews section and carries the same finding table
  and file:line citations as before. `--approve` and `--request-changes` are
  explicitly not used — both fail for a PR authored by the same identity the
  agent runs as; a failed review post is now reported as a loud failure
  rather than silently falling back to a plain comment.
- The 42 bundled agent `.md` assets moved from `src/assets/agents/` to
  `trusty-agents-common`, and `core::bundle` now embeds them from
  `trusty_agents_common::agent_assets`. Every `pub const` in `core::bundle`
  keeps its name and content, so nothing downstream changes. Edit a bundled
  agent at `crates/trusty-agents-common/src/assets/agents/<name>.md` from now
  on; the whole roster moved together because `extends:` resolves within a
  single directory.

### Removed

- tm no longer writes MCP server declarations into a session's workspace
  `.mcp.json`, and no longer pre-approves any MCP server name via
  `enabledMcpjsonServers`. The five injectors, their two call sites, the
  `.mcp.json` git-exclusion guard they needed, and the whole trust derivation
  behind them are deleted. MCP servers are declared once in
  `<CLAUDE_CONFIG_DIR>/.claude.json`'s user-scope `mcpServers` map — where
  `tm mcp add` already writes and where a relocated spawn reads them with no
  approval prompt. An approval left by an earlier version is stripped from the
  project entry on the next launch: an approved name is what lets a repo's own
  `.mcp.json` entry override the operator's declaration, so leaving stale ones
  in place would keep that displacement alive (#4181, ADR-0042).

### Documentation

- The `tm-adr` bundled skill's numbering convention now names a claim-then-populate
  protocol: check `docs/adr/` on `origin/main` (not a possibly-stale local
  checkout) and any in-flight branch or open PR before picking a number, create
  the ADR file first as a stub reserving that number, then populate it. Also
  states what to do on discovering a collision — report it, never silently
  renumber another author's ADR. Closes a gap two same-day collisions exposed:
  ADR-0021 (pre-existing, out of scope here) and a same-day ADR-0038 collision
  caught only by an alert author.
- Corrected `prune_vanished`'s doc comment in `core/mcp_provenance.rs`, which
  claimed the sweep "retains only entries whose path still exists"
  ([#5551](https://github.com/bobmatnyc/trusty-tools/issues/5551)). It also
  retains an entry it could not probe — a deliberate fail-open, since dropping
  an uncertain provenance claim risks misattributing a later `.mcp.json` at the
  same path. Doc-only; the behavior is unchanged.

## [1.3.5] — 2026-08-10

### Added

- `BASE-AGENT.md` (synced to `trusty-code`) adds a "Never Directly Monitor a
  Declarative Process" rule: run gates in their quiet/filtered form
  (`--quiet`, `tail`/`grep` for the summary line, or `tm compress --tool
  "<tool-name>"`), re-run only the failing case with full output, and never
  `gh pr checks --watch`. Prompted by two agents burning 415k and 546k tokens
  streaming per-test and per-check-poll output directly. States explicitly
  that filtered output is still raw output — the evidence rule is unchanged
- `BASE-AGENT.md` (synced to `trusty-code`) now states a "No Subagent
  Fan-Out" rule: a delegated agent does its own work or reports back to its
  dispatcher — it never spawns its own subagents. The Agent/Task tool is
  reserved for the top-level PM/orchestrator. Prompted by a `rust-engineer`
  spawning an untyped child agent that bypassed the roster entirely for
  routine documentation work
- `BASE-AGENT.md` (synced to `trusty-code`) adds "Agent-Authored Prose",
  extending the PM's "Write Plainly" register (`core.md`,
  [#4757](https://github.com/bobmatnyc/trusty-tools/issues/4757)) to the
  agent side: review verdicts, reports back to the PM, ticket/PR body text,
  and generated documentation — lead with the concrete referent, state cause
  then effect, show before-and-after, cut hedges and process narration, end
  options as a bare enumeration. Governs how, never whether: the evidence
  rule is unchanged
- `BASE-AGENT.md` (synced to `trusty-code`) Agent-Authored Prose adds "never
  announce the register you're writing in" — no heading or preamble that
  labels the writing as plain, honest, direct, candid, blunt, or unvarnished
  (e.g. "What remains unknown, stated plainly:"); state the fact in place
  instead. Same family as the banned word "honest" — the label is forbidden,
  not the disclosure
- `core::claude_md_writer` — the single owner of `CLAUDE.md` section-override
  writes, the write-side counterpart to the reader-only
  `core::claude_md_sections` (refs [#4754](https://github.com/bobmatnyc/trusty-tools/issues/4754))
  - `write_section_override` is idempotent by construction: a repeat write
    replaces its block in place and collapses any pre-existing duplicate, so a
    section can never accumulate a stacked second copy
  - `core` is refused because the shipped package declares it `fixed` — the
    writer asks `CustomizationTier::permits`, it does not carry its own list
  - every refusal (protected section, empty body, a marker line inside the body,
    an unpaired marker in the host, an unreadable host) leaves the file
    byte-identical and the bundled section in force
  - written blocks adopt the host's dominant line ending, so splicing into a
    CRLF `CLAUDE.md` does not leave a mixed-ending file
  - `ensure_compiled_pointer` records where the composed PM prompt lands, using
    delimiters deliberately outside the `TRUSTY-MPM:` grammar so the reader sees
    no override and raises no diagnostic; it collapses duplicate pointer blocks
    the same way section writes do
- bundled `framework-manifest.toml` — the framework tier of the existing
  `manifest.toml` format — now declares which agents deploy, replacing the
  computed "everything not in `LANGUAGE_ENGINEERS`" rule
  (closes [#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - four deployment categories: `universal` (no detection), `language`,
    `framework`, and `platform` (marker-gated), plus `deprecated`
  - a missing, malformed, or non-exhaustive framework manifest fails loudly
    rather than falling back to deploying everything or nothing
  - `tm generate capabilities` now sources the agent reference's deployment
    category from the manifest
- new `elixir-engineer` agent, gated on `mix.exs`, covering general Elixir and
  OTP work — supervision trees, GenServer processes, Ecto, ExUnit
- New bundled `tm-slack` skill folding `tm-slack-canvas-delivery` into general Slack delivery guidance — routes through the native `mcp__slack-mcp__*` connector per ADR-0014, enumerates the verified 22-tool surface, and generalizes the "creation is not delivery" completion rule to every delivery shape (message, canvas, scheduled) (closes [#4761](https://github.com/bobmatnyc/trusty-tools/issues/4761))
  - registration into `bundle_all.rs`'s `ALL` table is a separate, sequenced follow-up (that file was held by a concurrent PR); the skill does not ship until that line lands
- `tm hook --pm-guard` now denies `Task`/`Agent` dispatch when the calling session is itself a subagent (closes [#4784](https://github.com/bobmatnyc/trusty-tools/issues/4784))
  - the PM keeps dispatching; only fan-out from within a subagent is blocked
  - `SendMessage` is never denied, so a blocked agent can always report back
  - fails OPEN — an indeterminate caller context allows the dispatch rather than risking a false deny against the PM
- Per-subagent context-cost guard at the `PreToolUse` hook ([#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837)) — a subagent's accumulated context is measured from its own transcript and classified against the new `[agent_cost]` section of `~/.trusty-mpm/config.toml` (`enabled`, `warn_tokens`, `max_tokens`)
  - ships WARN-ONLY: warns at 250k, and the hard stop is opt-in (`max_tokens = 0` by default). Sized against the measured population rather than one sample — across the 600 most recent subagent transcripts on a working machine over 14 days the distribution is p50 136k, p90 268k, p95 323k, with 18/600 (3.0%) at or above 400k, so a shipped-on 400k ceiling would deny roughly one dispatch in 33
  - the warning reaches the agent, once, as `hookSpecificOutput.additionalContext` — no `permissionDecision` is emitted, so the tool call still goes through the normal permission flow. "Once" is per agent, keyed on `agent_id` or, when the payload carries only the subagent's transcript path, on that path — siblings of one parent no longer share a key, where previously the first to warn silenced the rest; a key that cannot be derived fails open and keeps informing the agent
  - when an operator opts into the stop, `SendMessage` and `git add`/`commit`/`push`/`status`/`diff` stay permitted so a stopped agent can always save and report its work; every segment of a composed Bash command must be one of those, so work cannot be smuggled behind an allowed verb
  - that allowlist judges the flags, not just the subcommand name, and is default-deny on anything it does not recognise: git global options other than `-C`/`--git-dir`/`--work-tree`/`--no-pager`/`--no-optional-locks`/`--literal-pathspecs` are rejected (so `-c`, `--config-env` and `--exec-path` cannot inject `diff.external`, `core.gitProxy`, `credential.helper`, `protocol.ext.allow` or an alias), the program must be `git` itself with no env-assignment/`sudo`/`env` prefix, any unquoted `$`/`` ` ``/`(`/`)`/`<`/`>` disqualifies the whole command (closing process substitution and redirection in both directions), any `::` token is rejected as a remote-helper transport (`ext::sh -c …`), and the `--exec`/`--*-pack`/`--ext-diff`/`--textconv`/`--output` families are rejected wherever they appear
  - measured from a bounded tail read (64 KiB, retried once at 1 MiB when the smaller window holds no complete usage record), so the check costs the same on a 1 MB transcript as on a 500 MB one
  - fails open at every step: the PM is never evaluated, and an unreadable transcript, a missing usage record, or `max_tokens = 0` all allow the call
- `tm-delegation-patterns` now carries two concurrent-dispatch rules: every brief must name the files owned by other in-flight branches ("we stack, we do not race"), and a running agent's scope is fixed — new work is a new agent, because cost tracks accumulated context over the agent's lifetime ([#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837))
- `BASE-AGENT.md` bans ending a gate chain in a pipe — `cargo test … | tail` and `cargo test … | tm compress` both exit 0 on a failing suite — and gives the canonical redirect-then-echo form
- `tm memory import <DIR> --palace <SLUG>` — bulk-load a directory of memory
  `.md` files into a trusty-memory palace with zero LLM inference
  (refs [#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837),
  unblocks [#4834](https://github.com/bobmatnyc/trusty-tools/issues/4834))
  - maps YAML frontmatter onto drawer fields: the `description` leads the
    stored text, and `name` + `metadata.type` + every `[[wikilink]]` target
    become tags
  - `--dry-run` reports what would be written and issues no writes;
    `--json` prints a per-file report carrying each drawer id
  - re-running never writes a file twice: the file's own drawer is found by
    its slug tag with linking drawers excluded structurally (their wikilink
    targets are re-derived), so a file whose text has drifted since it was
    imported is still recognised and skipped rather than duplicated. Never
    `memory_recall` ([#4836](https://github.com/bobmatnyc/trusty-tools/issues/4836))
  - fails closed instead of risking a duplicate or a truncated fact: an
    ambiguous candidate set, a slug tag shared by more drawers than one
    `memory_list` page returns, a frontmatter line with no `key:` separator,
    and YAML's plain multi-line scalar form are all reported as per-file
    failures rather than imported
- Two prose rules in the PM's "Prose Style — Write Plainly", mirrored into `BASE-AGENT.md`: no praise for the user (acknowledge with "OK" or disagree and say why), and delete the framing opener and lead with the fact. Both state the ban as a category or template with their examples explicitly marked non-exhaustive, because a literal example list had already failed to generalize
- `tm reinstall` — redeploys the bundled agents and skills to EVERY deploy destination, not just the one the command you happened to run writes. It refreshes the compiled-in bundle onto disk first (hop 1), then deploys into the tm-managed `claude-config`, the operator's `~/.claude/skills`, the current project's `.claude/skills`, and the standalone driver's `~/.trusty-mpm/claude-config` — the last of which no command reached before (closes [#4849](https://github.com/bobmatnyc/trusty-tools/issues/4849)). Reports deployed / repaired / preserved / unchanged / failed per destination.
  - Ownership is the deployers' existing model: a framework-owned file that drifted or was corrupted is repaired, a file you own or edited is preserved. `--force` overwrites, backing every file up first.
  - The binary is never touched unless `--binary` is passed, and the route is decided by install provenance — a crates.io install upgrades through the shared update path, a `cargo install --path` install rebuilds from its recorded source directory, and anything unclassifiable refuses instead of guessing.
  - A skill a higher-precedence tier owns the name of is counted and named in the report. The tier planner drops it before the deployer runs, so it reaches none of the deployed/preserved/unchanged/failed counts — reporting only those printed zeros for a bundled skill that was never deployed. A bundled-named skill the destination does not track ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)) additionally names `tm reinstall --force`, which adopts exactly that set.
  - The project tier is refreshed only when `<cwd>/.claude/skills` already exists, so running `tm reinstall` from any directory never creates a `.claude/` there, and a failure at that destination is reported without failing the command.
  - A skill several tiers carry is counted once, not once per shadow record.
- `tm-capabilities` gains a generated `references/framework.md` — the install layout, agent tier precedence, skill deploy tiers, per-session state, and an existence-checked index of the authoritative docs (separating what ships in the published crate from what is repo-only). Rendered from the path constants and tier resolvers the runtime uses, so moving a directory or reordering a tier drifts the committed skill and fails `scripts/check_capabilities.sh` (closes [#4946](https://github.com/bobmatnyc/trusty-tools/issues/4946))
  - the compiled prompt's agent-precedence block no longer names `~/.trusty-mpm/agents/`, a tier no code reads; it now states the real order including `$CLAUDE_CONFIG_DIR/agents/`
- `tm doctor --fix` repairs the findings tm can prove it owns, instead of only reporting them (closes [#4948](https://github.com/bobmatnyc/trusty-tools/issues/4948))
  - dry run by default: it prints every change per item, with paths, and writes nothing until `--yes`
  - covers `skill_staleness` (redeploy from the bundled asset), `hooks_contamination` (strip tm's own hook entries from this project's `.claude/settings*.json`, backing the file up first), and `push_guard` (retrofit the cross-branch `pre-push` guard)
  - `legacy_sources` findings under `~/.claude` are reported as REFUSED and never deleted — a copy there may be hand-edited, and a directory name cannot prove otherwise
  - a hand-edited (FROZEN) skill and a foreign `pre-push` hook are refused in both modes; `--include-frozen` now works with either `--fix` or `--fix-skills`
  - `--fix-skills` gained a preview through `--fix`, and its output now names the exact file it repaired at each tier
- Fail-Open Check added to the bundled PM workflow section — a five-question review gate for any failure branch that advances a cursor, watermark, index or "done" marker past the thing that failed
- `tm shell-init <zsh|bash|fish>` prints a `tm` wrapper function so your shell is left in the alias's repo after `tm run <alias>` exits ([#4986](https://github.com/bobmatnyc/trusty-tools/pull/4986))
  - install it yourself: `eval "$(tm shell-init zsh)"` in `~/.zshrc`, or `tm shell-init fish | source` in `config.fish`. The command is print-only — nothing in trusty-mpm writes to a shell rc file
  - a process cannot change its parent shell's cwd, so before this a session that ran in another repo left the shell behind and a later bare `tm` re-detected the wrong project
  - the wrapper resolves through the existing `tm path <alias>`, which derives the path from `(alias, root)` and never persists it; there is no state file or env-var channel
  - it passes the real exit status through unchanged, `cd`s only for `tm run`, and stays silent and stationary when `tm path` fails, prints nothing, or names a directory that is gone
- `tm run <owner>/<repo>` cold-starts a daemon-managed session for a repo that is not on disk yet
  - clones to the managed checkout at `~/trusty-mpm-projects/<owner>/<repo>`, then hands off to the existing `tm launch` path — so the result is a real `SessionRecord` with a tmux pane, visible in `tm ls` and `tm sessions`, not a blocking foreground `claude`
  - the positional is classified by the same predicate `tm register` uses, so `tm run bobmatnyc/trusty-tools` and `tm register bobmatnyc/trusty-tools` cannot disagree about what a string means; a registered alias keeps the unchanged DOC-24 standalone behaviour
  - reusing an existing managed checkout FAILS LOUD when its `origin` names a different repository — no auto-repoint, and the refusal lands before anything writes to the directory
  - a dirty existing checkout is not an error: the fast-forward is skipped and said so in normal output, while the fetch still runs. The session's branch is cut from a freshly-fetched `origin/<default-branch>`, so it never inherits the checkout's local `HEAD`
  - `tm run ./some/dir` now reports that a relative path is not a repository instead of "alias not found"
- `tm f [pattern]` — find a session by name, filtering as you type
  - the list stays on screen and narrows per keystroke against the session NAME only; `[pattern]` seeds the filter box. Up/Down select, Enter opens the highlighted session through the numbered picker's own resume path, Backspace edits, Ctrl-U clears, Esc cancels
  - `tm ls <term>` is unchanged: still the one-shot, pipeable listing, still matching every visible column (id, name, project, state, task). `tm f` is the narrow lookup — a task description mentioning "api" no longer pads the answer to "which sessions are CALLED api-something"
  - piped output, `--json`, `--all`, or `TERM=dumb` never enter raw mode; they print the same static filtered table `tm ls` would. The gate is checked before raw mode is requested, so a script that pipes `tm f` cannot hang on a keyboard nobody is at
- `tm ls` colors the `ID` column, dimmed — it was the widest cell on the row and the only uncolored one, so the table read as a wall of undifferentiated UUID. `NUM`, `ID`, and `NAME` now carry three distinct hues, all suppressed on a pipe and under `NO_COLOR` as before
- `[hooks] prompt_context = false` in `~/.trusty-mpm/config.toml` turns off the per-prompt `trusty-memory prompt-context` injection (closes [#5034](https://github.com/bobmatnyc/trusty-tools/issues/5034))
  - the hook costs a measured ~1,211 tokens on every prompt (median 1,252, range 693–1,438 across 1,114 firings over five days) — roughly 24,000 tokens across a 20-turn session, recurring — and [#4904](https://github.com/bobmatnyc/trusty-tools/issues/4904) measured 0 clean matches out of 17 curated facts on that same corpus. There was no way to stop paying it: the hook block is a hardcoded const written unconditionally at every session launch, so a hand edit to `.claude/settings.json` was overwritten on the next launch
  - default is `true`. An absent `[hooks]` section, and a present-but-empty one, both leave the write byte-identical to before
  - only the `UserPromptSubmit` entry is suppressed. `SessionStart` → `trusty-memory inbox-check`, the `PreToolUse` PM guard, and the six-event lifecycle triad are written either way
  - the strip that removes trusty-mpm's own prior entries now covers every event trusty-mpm owns rather than only the ones the current config writes. Without that, the key would have done nothing on any project already launched once — the stale `UserPromptSubmit` entry would have stayed in `.claude/settings.json` and kept firing
  - config file only, by design: no CLI flag and no environment variable
  - `config.rs`'s inline test module moved to a sibling `config_tests.rs` (no test changed) — the new section pushed the production file to 533 SLOC, over the 500 cap
- `tm doctor` gains a `search_index_pin` check that resolves the trusty-search index a session is actually pinned to ([#5045](https://github.com/bobmatnyc/trusty-tools/issues/5045))
  - session launch writes `trusty-search serve --index <id>` into the project's `.mcp.json` and registers that index best-effort. Every step of the registration swallows its failures at `warn!`, so the pin advances even when index creation never happened. The existing `search` check asks the daemon whether it is healthy and whether the *derived* id appears in `/indexes`, so it kept reporting fine: 4 of 75 live worktrees had an index, while a bare `search` in the rest returned `404 unknown index`
  - the new check reads the pinned id out of `.mcp.json` and resolves it with `GET /indexes/{id}/status`. A 404 is `Fail` and names the id; an index that resolves with 0 chunks is `Warn` (registered, never populated); an unanswered daemon is `Unknown`, never `Ok`
  - read-only — it never creates or reindexes anything
- a worktree is now registered with trusty-search at the moment it is created, instead of only when a session launches in it ([#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
  - covers both worktree-minting paths: the in-project managed-session path (`daemon::managed_routes::inproject::create_session_worktree`) and the clone-provisioned path (`provisioner::workspace`'s `RealGitBackend::worktree_add`)
  - the index is BM25 + KG only — no embeddings. Exact text and the symbol graph are branch-specific and must be worktree-accurate; conceptual similarity is not, so the expensive lane stays on the base checkout
  - fire-and-forget on a detached thread: worktree creation and session launch never wait on indexing at any repo size. A cold BM25+KG build of this workspace (5,865 files, 75,282 chunks) measured 35.0 s
  - idempotent — a repeat call against an already-indexed worktree is two short HTTP round trips and no walk
  - only a real git worktree is indexed; a plain clone is refused, so worktree creation never makes an operator's opt-in decision for them
  - teardown is unchanged: `session_manager::search_gc` already derives the same index id and deletes it at decommission, with an orphan sweep behind it
- `tmux.alternate_screen` in `~/.trusty-tools/trusty-mpm/config.yaml`, alongside `history_limit` and `mouse` ([#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151)). It defaults to `true`, tmux's factory value and today's behaviour; setting it to `false` stops panes using the terminal's alternate screen buffer, which is what makes a full-screen TUI's output land in scrollback instead of being discarded. This is why a `tm` session can verify a 100,000-line `history-limit` and still have nothing to scroll back through — the history was never written to the pane.

  Turning it off is server-wide in effect: every trusty-* managed session shares one tmux server, so `vim`, `less`, `htop` and `man` also stop restoring the screen they covered, each leaving its final frame smeared into the scrollback on exit. It is not retroactive either — anything already written while the alternate screen was up stays unrecoverable.

  The option rides the existing verified apply path (`start-server` → `set-option` → `show-options` readback, retried, before `new-session`), and its readback queries the same `-wg` scope the set writes, so a wrong-scope set cannot verify green.

- `TmuxConfig` and `ResolvedTmuxOptions` are now `#[non_exhaustive]`, so this field addition — and every future one to a section designed to grow — is not a SemVer break. Construction from outside the crate goes through `Default` plus field assignment or `resolve_tmux_options`; field reads are unaffected. Same treatment `trusty-common`'s `IndexOptions` got in [#5065](https://github.com/bobmatnyc/trusty-tools/pull/5065).
- The session-worktree base directory is configurable via `worktrees_dirname` in `~/.trusty-tools/trusty-mpm/config.yaml` or `TRUSTY_MPM_WORKTREES_DIRNAME`, defaulting to `.worktrees` (#5204). Creation uses the configured name; detection also keeps matching `.worktrees`, so retargeting never orphans worktrees already on disk.
- trusty-mpm now reads a committed, project-level config file (closes [#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - `<project>/.trusty-mpm.toml` is tracked in git, so a project's conventions travel with the clone, show up in PR diffs, and stop being something every operator has to re-declare on every machine. It is a ROOT-LEVEL dotfile rather than a member of `<project>/.trusty-mpm/`, because that directory holds machine-local session state and projects gitignore it wholesale — this repository's own `.trusty-mpm/*` rule already makes the #4832 `framework/manifest.toml` layer untrackable, and trusty-mpm cannot carve a re-include out of a consumer's `.gitignore`
  - `worktree` is the first setting to use it. Precedence, highest first: the project's `.trusty-mpm.toml`, then the machine-global `projects.json` registry, then the built-in `true`. The project layer wins in both directions — a repo can force isolation back ON over a local opt-out, not only off
  - the clone-based spawn branch, which asks the question before any project directory exists, is unchanged and still resolves from the registry alone
  - a key the schema does not define is REJECTED (`serde(deny_unknown_fields)`), so `worktre = false` is an error instead of silently reading as "worktrees stay on". At spawn time a rejected file contributes nothing and is logged at `error` level, and resolution falls through to the registry — a committed file is shared, so one bad push must not brick every operator's launches
  - `default_model` is the second setting: it tops the same chain `resolve_agent_model` already ends in. Explicit `--model`, per-agent overrides, and agent frontmatter are more specific than a default and still win over it
- `tm launch --worktree`, and the matching `worktree` field on
  `POST /api/v1/sessions/managed`: the launch-time request that puts one
  specific session in its own protected clone plus worktree, overriding the
  main-checkout default.

### Fixed

- `code-review-standards` and the `code-critic` agent (both synced to
  `trusty-code`) no longer let `Promote` act as a filing engine. The default
  is now stated explicitly: a review finding is fixed in the surfacing PR, or
  dropped. `Promote` is reserved for defects that are genuinely separable,
  schedulable work — not every LOW/MEDIUM finding — and the critic never
  files an issue itself or instructs anyone to; it only recommends, and the
  PM or user decides. Prompted by nine issues filed in a single review leg
  against one merged PR, several forwarded straight from an unqualified
  `Promote` disposition
- `agent-delegation.md` no longer claims code-critic is a "universal qa-tier
  agent" for "any engineer dispatch" or design critique — both were owner
  rulings against the actual standard. Added a "code-critic Dispatch
  Standard" keyed to the project's test-ladder rung (never on rungs 1–3,
  conditional on rung 4, required on rungs 5–6), plus explicit escalations
  and non-reasons to dispatch
- `code-production-process` (and its `stage-architect.md`/`stage-critic.md`
  references, synced to `trusty-code`) no longer describes an "optional
  Phase 2 design critic pass" at the Architect stage — that contradicted the
  standard; early interface review routes to `code-analyzer` instead
- A test in `pm_guard_bash` set `TMPDIR` to a machine-specific path with `std::env::set_var`, reddening unrelated tests across the whole repo at random. `cargo test` runs tests as threads in one process, so the variable was visible to every concurrent sibling; `tempfile` honors it, so `pm_guard_budget`'s tempdir calls tried to create directories under a macOS-only path and panicked `NotFound` on the Linux CI runner. It failed PR #4914's Test job (run 31023632348) on code that PR never touched. The expansion rules are now asserted against an injected `PathEnv` instead of process env — no global mutation remains anywhere in the workspace — and `pm_guard_budget`/`pm_guard_cost` take their temp dirs from a new hermetic `test_support::hermetic_temp_dir` for the `tm` bin target, which roots under the hardcoded system temp path and ignores `$TMPDIR` entirely.
- `tm catalog apply --prune` now reads an agent's ledger ownership tier, not just its checksum (completes the agent half of [#391](https://github.com/bobmatnyc/trusty-tools/issues/391))
  - the agent manifest records an `Origin` the skill manifest lacks, and `agents::deployer` already honours it — a non-`Bundled` entry is the seed-once tier and is preserved on a checksum mismatch. Prune read the checksum and not the tier, so a pristine user- or registry-owned agent could still be deleted by a bundled exclude rule: the hole the skill side closed by deriving the user tier from its source directory
  - latent today, because nothing writes a non-`Bundled` origin; pinned by a regression test that constructs the ledger entry directly
- `tm catalog apply --prune` no longer deletes user content (closes [#391](https://github.com/bobmatnyc/trusty-tools/issues/391))
  - it removed any managed skill directory the current include/exclude rejected, with no checksum comparison, no frozen check and no backup — the only skill-mutating path with none of the guards `deploy_one_file` and `skill_repair` already have
  - a hand-edited (frozen) agent or skill is now left in place, as is a skill directory holding any file trusty-mpm did not deploy — `remove_dir_all` would have taken that file too
  - a skill the user-custom tier supplies is never pruned, whatever the bundled include/exclude says: `deploy_all_skill_tiers` deploys that tier in full and exempt from those rules, but its ledger entry is indistinguishable from a bundled one, so a checksum gate alone waved a pristine user skill through. The tier is derived from the live `~/.trusty-mpm/skills/` source rather than a new manifest field, so no existing ledger entry has to be guessed at
  - everything actually deleted is copied to `~/.trusty-mpm/backup-catalog-prune-<timestamp>/` first, and the command now prints what it kept and why
  - prune judges skill STEMS, not raw ledger keys: an `include` rule matching a stem but not its carried `<stem>/references/*.md` key used to drop that file's ledger entry while leaving the file on disk, after which the deployer read it as user-owned and never updated it again
- Updated two doc comments that still named `open-mpm` to say `trusty-agents`
  (renamed in #831).
- managed sessions no longer dirty a git-tracked `.mcp.json`, and this repo stops tracking one (refs [#4181](https://github.com/bobmatnyc/trusty-tools/issues/4181), closes [#5058](https://github.com/bobmatnyc/trusty-tools/issues/5058))
  - every entry the MCP injectors write is machine- or session-specific: `trusty-search`'s `--index` pin names the ephemeral worktree, and the `tm mcp add` bridge can carry an absolute `command` path valid on one machine. While the file was tracked, each managed session started dirty and a routine `git add -A` could commit an operator-specific path into the repository
  - `prepare_session` now adds `.mcp.json` to `.git/info/exclude` before any injector writes to it, reusing the helper the `.env.local` secret route already used (`ensure_env_local_git_excluded` is now the filename-parameterised `ensure_git_excluded`). Non-fatal and deliberately not fail-closed — skipping injection would drop the force-overwrite that #3934/#3950 require before a builtin name may enter `enabledMcpjsonServers`
  - `runtime::claude_code::prepare_managed_config` — the second site that runs the same four injectors — excludes the file too. Two of its three callers (`spawn_resume`, `build_inplace_resume_command`) reach it with no `prepare_session` anywhere in their chain, so without this every resume re-dirtied the file
  - a tracked `.mcp.json` now produces an operator warning naming `git rm --cached`. The exclude write silently succeeds on such a repo, since `info/exclude` never applies to a path already in the index, so the failure arm alone left that case with no signal at all
  - `info/exclude` cannot untrack a path already in the index, so this repo's own `.mcp.json` is removed from it. `.gitignore` had declared the file machine-local since before this change; it was tracked only because `git rm --cached` was never run. That also removes the committed `tickets-mcp` registration: no install path builds `trusty-common`'s bin targets, so the binary was never on `PATH` and every session reported it as `✘ ENOENT`
  - the shared injector write path now ends `.mcp.json` with a newline, so a repository that does track it no longer sees `\ No newline at end of file` on every rewrite
- `register_project_index_never_bypasses_sensitive_path_denylist` opts into real daemon writes with `TRUSTY_ALLOW_PRODUCTION_STATE=1`, so it keeps receiving the `POST /indexes` body it asserts on now that test processes are barred from writing to a live trusty-search daemon (refs [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - the override is scoped to the test and points at its own loopback socket, never the operator's daemon
- a stale project-tier agent file that shadows a bundled agent is now quarantined on session launch and on `tm sessions sync-assets`, instead of silently winning the resolution race forever (closes [#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448))
  - `retract_framework_agents` (#4409) can only delete what its ownership ledger names, so a copy written before that ledger existed survived it, outranked the canonical user tier, and was never refreshed again — [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) made permanent
  - the sweep runs AFTER retraction, against the workspace's OWN `.claude/agents` — never `fw.claude_agents_dir()`, which is the operator's real `~/.claude/agents` on the non-git `tm session start` and TUI `/connect` paths
  - a file moves only on a POSITIVE answer from every gate: it resolves to a bundled name, the ownership ledger does not record it as the operator's, git gives a definitive answer that nothing claims it — either a readable work tree that does not track it, or no repository at all — and its frontmatter and body are trusty-mpm's own composer output. Anything less — a tracked file, an unreadable or ceiling-blocked repository, a bare repo, a stale worktree pointer, git absent, a claude-mpm artifact, a hand-authored file — is left alone and recorded in the receipt with the reason
  - it never deletes: each moved file keeps a verified byte-identical backup under `.trusty-mpm/agent-quarantine/` plus an inert `.md.disabled` sibling, and a receipt records how to restore it
  - the bundled-name roster moved to `core::bundled_roster`, so `tm doctor`'s `asset_tier` probe and the quarantine resolve it from one place and cannot drift on what counts as canonical
- `research`'s deployed `skills:` list now matches its source asset — `tm-capabilities` was inherited from `BASE-AGENT` and silently appeared only in the deployed copy (closes [#4643](https://github.com/bobmatnyc/trusty-tools/issues/4643))
  - DOC-42 now documents that `skills:` unions base-first across the `extends:` chain, so a foundation template's declaration is paid for by every descendant
- `tm ls` auto-prune no longer misses stopped sessions or non-TTY invocations, so dead records stop accumulating (closes [#4702](https://github.com/bobmatnyc/trusty-tools/issues/4702))
  - every listing surface prunes now — piped, scripted, `--json`, `tm session ls`, and bare `tm` — not just the interactive TTY picker
  - a `stopped`/`errored` record whose workspace the CLI independently verifies is gone is cleared, closing the gap where a display-reconciled zombie (persisted `active`, tmux pane gone) never got the daemon's `unresumable` probe
  - a stopped record whose workspace still exists on disk is never cleared, and `decommissioned` records, `attached` records, and any record naming a live tmux session are untouched
  - a workspace is only "gone" when its parent directory still exists, so an unmounted volume no longer reads as every session on it being dead
  - confirmation requires 10 minutes of real elapsed age since the FIRST sighting; an intervening listing no longer resets that clock, which would otherwise leave auto-prune inert under any `tm ls` cadence tighter than the window
  - when tmux cannot be enumerated at all, nothing is pruned — without a liveness signal an `errored` record with a live pane could be tombstoned into a terminal state
  - the prune clears registry records only: never a git worktree, a branch, a file on disk, a live runtime, or a search index
- the delegation roster no longer admits prompt fragments as delegatable agents (closes [#4711](https://github.com/bobmatnyc/trusty-tools/issues/4711))
  - `is_foundation_file` now matches a bare `base.md`, not only `base-*.md`
  - a file with no `name:` frontmatter is excluded outright — Claude Code dispatches by `name:`, so the old file-stem fallback advertised targets the harness could never resolve
- Worktree teardown no longer deletes worktrees git refused to delete (closes [#4732](https://github.com/bobmatnyc/trusty-tools/issues/4732))
  - `remove_session_worktree` fell through to `std::fs::remove_dir_all` on ANY non-zero `git worktree remove --force` exit. Git exits 128 for every fatal condition, so `git worktree lock` — the operator's only "do not remove this" — was exactly what caused deletion. A stale worktree pointer, an unreadable `.git`, and a repository git merely declined to read all produced the same outcome, with uncommitted work in the worktree.
  - Every git failure is now classified into three states — git holds state here / git positively holds nothing here / git could not be asked — and only the middle one permits a raw removal. Locked, stale-pointer, broken-`.git`, and unrecognized-message cases are refused. Reachable from `tm session decommission`, `tm sessions prune --state stopped`, the age-based reaper, `prune_orphaned_worktrees`, and the `--merged-prs` reclaim pass.
  - The refusal reason is now returned to the caller instead of buried in a log line: `tm session prune-worktrees --merged-prs` reports `removal_failed` entries as `"<path>: <reason>"`.
- `cargo test` can no longer destroy a real trusty-search index (closes [#4743](https://github.com/bobmatnyc/trusty-tools/issues/4743))
  - `session_manager::search_gc` formatted `DELETE /indexes/{id}?delete_data=true` at two sites and sent it to whatever daemon `resolve_daemon_base_url` discovered — under a test run, the operator's live one on port 7878. `?delete_data=true` destroys the index's on-disk data directory
  - fixture workspaces derive their index id from a bare `file_name()`, so `decommission_full_still_terminates_the_runtime` asked that daemon to destroy an index named `full`; `sess`, `live` and `proj` appear the same way elsewhere in the suite
  - both sites now go through one `DestructiveIndexDelete` capability that holds the only copy of the `?delete_data=true` literal and exposes no constructor taking a base URL, so a caller cannot build the request without acquiring it — and a `cargo test` process never can
  - the orphan sweep acquires the capability before listing anything, so a test process makes no request at all instead of enumerating real indexes and stopping at the delete
  - `TRUSTY_ALLOW_PRODUCTION_STATE=1` remains the explicit opt-in for a test that deliberately drives a real daemon
- the compiled PM prompt is now refreshed at the spawn seam, so resume, guided-resume and crash-recovery launches no longer run a prompt that never reached `INSTRUCTIONS-COMPILED.md` (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the compiled write is the last step of session preparation, so a failure can no longer skip the MCP content-pinning injectors that defend against the #3918/#3950 name-squatting class
- compiled PM system prompt now writes to its own `INSTRUCTIONS-COMPILED.md` instead of colliding with the bundled `instructions/INSTRUCTIONS.md` stub that the pipeline reads as input (closes [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the compiled prompt is refreshed at every session launch, before `claude` is spawned; a failed write now aborts the launch rather than leaving a stale prompt on disk
  - `tm install` removes a stale pre-#4752 `framework/instructions/INSTRUCTIONS.md`, which no writer could refresh but the pipeline still read
- PM instruction section `non-overridable-rules.md` no longer describes section overrides as "override files" — the live mechanism is marked blocks in the project's `CLAUDE.md`, and the section now points at the spec of record (refs [#4753](https://github.com/bobmatnyc/trusty-tools/issues/4753))
- a session whose instructions cannot be written no longer starts (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - `build_instructions` returned a non-fatal error, so every launch path logged it and started the session anyway — with no instructions established and no `.mcp.json` written. Reachable whenever the project's `CLAUDE.md` or the framework instructions path cannot be read, for example when something has left a directory at `CLAUDE.md`
  - it is now the same fatal condition as the compiled-prompt write, reported as one error rather than two classes
- bare `tm` in a managed pane now refreshes the project's compiled prompt before relaunching, and refuses the relaunch if that write fails (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - this path calls neither session preparation nor the daemon resume route, so it was the one way to start a session on a stale or missing compiled prompt where a fresh launch would have refused
  - the two resume-shaped paths now share one `refresh_compiled_prompt` entry point; session preparation keeps its own composition because it must apply the operator's resolved output style
- an unwritable `.trusty-mpm/` no longer causes a session to start without its instructions recorded (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - the `last-instructions.md` stash write degraded the whole preparation to an early non-fatal error, so callers launched the session having skipped the fatal write below it
  - the stash is an inspection copy; it now logs and continues instead of short-circuiting
- the `CLAUDE.md` compiled-instructions pointer no longer names the retired global path (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - it pointed at `~/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`, which nothing writes since the compiled prompt became project-local; it is now the project-relative path, pinned against the pipeline that writes the file
- The TUI health panel's palace detail showed "Wings: N" from a field that has
  always carried a ROOM count (closes
  [#4811](https://github.com/bobmatnyc/trusty-tools/issues/4811), ADR-0027 C3.4).
  It now reads the daemon's truthful `room_count` and renders "Rooms: N",
  falling back to the legacy `wing_count` when talking to a daemon that predates
  the change — where that field held exactly this number.
- `tm session start` from a directory outside any git repository is now refused with an actionable error instead of scattering `.trusty-mpm/`, a `CLAUDE.md` stub and a `.claude/` tier into whatever directory the shell happened to be in (refs [#4832](https://github.com/bobmatnyc/trusty-tools/issues/4832))
- session preparation no longer reads the retired `<framework_root>/instructions/INSTRUCTIONS.md`. Its content had no consumer, but any read error other than `NotFound` — a permissions fault, a directory at that path, invalid UTF-8 — raised the one fatal `PrepError` variant and killed the launch
- the `.gitignore` block `tm` writes into a target project now covers `.trusty-mpm/sessions/`, `.trusty-mpm/logs/` and `.trusty-mpm/last-instructions.md`. `framework/` and `config.toml` are deliberately still trackable — they are operator-authored config
- Agent frontmatter using a YAML block scalar (`description: >` or `|`) is now folded instead of taken literally. Five bundled writing agents — `copyeditor`, `pangram-editor`, `proofreader`, `writer`, `writing-critic` — rendered into the PM prompt as a bare `>`; the same defect would have silently truncated any block-scalar `role:` or `model:`
- Bundled agent assets now reach a **daemon-managed** session without a manual `tm install` (closes [#4840](https://github.com/bobmatnyc/trusty-tools/issues/4840))
  - managed session provisioning re-materializes `~/.trusty-mpm/framework/agents/` from the compiled-in bundle and deploys it, gated on a sha256 stamp so an unchanged bundle costs one file read
  - the standalone path (`tm run` / `tm load` / `tm login`) still routes through `global_config::ensure_global_config_dir`, which does not refresh — tracked as [#4849](https://github.com/bobmatnyc/trusty-tools/issues/4849)
  - a bundled-origin file whose checksum drifted is still overwritten (corruption, not user ownership); an untracked-and-differing or user-edited file is still preserved — but now surfaces in a bounded count-plus-preview warning carrying the `tm install --reset-agents <name>` pointer, instead of being skipped silently
  - agents that fail to compose (bad frontmatter, strict-YAML rejection) are reported in the same summary — those do not land at all, which is worse than stale
  - an uninitialized `agents/agents` submodule directory no longer wins the source resolution and deploys nothing; it falls back to the compiled-in bundle
  - the whole step fails open: a refresh or deploy that cannot run degrades to a warning and never blocks the session
- `tm ls` auto-prune now clears dead session records whose git worktree was fully removed (closes [#4872](https://github.com/bobmatnyc/trusty-tools/issues/4872))
  - `git worktree remove` deletes leaf and parent together, so the old immediate-parent probe read the removal as an unmounted volume and the record could never advance — it was never even marked in `auto-prune-seen.json`
  - absence is now corroborated against the nearest surviving worktree root (`.base/.worktrees`, `.claude/worktrees`); when that root is gone too, the record is still kept, so an unplugged external volume cannot mass-tombstone the sessions on it
- a skill the every-run deploy declines to refresh is now reported instead of skipped in silence: `ensure_managed_config_dir` emits one bounded warning naming the withheld files and pointing at `tm doctor --fix-skills --include-frozen` (closes [#4873](https://github.com/bobmatnyc/trusty-tools/issues/4873))
  - the skip itself is unchanged and still correct — a hand-edited (checksum-frozen) skill and an unmanaged project-custom skill are both preserved
  - corrects the `resume_managed` and `run_inplace_relaunch` comments that claimed agent/skill redeploy and MCP injection were not re-run on those paths; all three run paths reach `ensure_managed_config_dir` through `prepare_managed_config`, so they always were
- The project skill tier (`<workspace>/.claude/skills`) now redeploys on every run path when the binary version or the manifest's skill selection changes (closes [#4880](https://github.com/bobmatnyc/trusty-tools/issues/4880)). It was written only by `prepare_session` and `tm sessions sync-assets`, neither of which runs on resume or in-place relaunch — and because project outranks user, a skill an older binary left there silently shadowed the copy `managed_config` refreshes every run, the [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) shape one tier down.
  - A version bump re-runs deployment; the manifest never needs to see skill content. The manifest's `[skills]` selection is the second trigger because the operator's `<harness-root>/.trusty-mpm/framework/manifest.toml` can move between releases. Both are recorded in `<workspace>/.claude/skills/.trusty-mpm-project-tier-stamp`; a matching stamp writes nothing at all.
  - Custom skills survive the redeploy: a project-custom skill is dropped by the tier planner before any file I/O, and a checksum-frozen hand edit is skipped by the deployer.
  - A skill source that exists but holds no skill files — an unfetched `agents/skills` submodule looks exactly like this — is a refusal that leaves the stamp unwritten and warns, rather than recording an empty deploy as current.
- take the skill ledger lock in the two trusty-mpm writers that also read-modify-write it — `tm doctor --fix-skills` repair and `tm catalog apply --prune` (closes [#4881](https://github.com/bobmatnyc/trusty-tools/issues/4881))
  - a tier directory that does not exist is skipped before the lock, so a repair never creates an empty `skills/` directory in a project it was only inspecting
  - skill-drift auditing no longer counts the lock sidecar as deployed content when deciding whether a manifest-less tier is populated
- User-tier skills authored as a directory (`~/.trusty-mpm/skills/<name>/SKILL.md`) now deploy into every project (closes [#4949](https://github.com/bobmatnyc/trusty-tools/issues/4949))
  - they were silently rejected by the source scan, so `duetto-design-system` and `cto-kb-ingest` were hidden from every project's skill manifest
  - a skill directory with no `SKILL.md` is now named in the deploy warning instead of vanishing
- session worktrees are now branched from a freshly-fetched `origin/<default-branch>` instead of the base checkout's local `HEAD`, which left every new session as stale as the operator's last `git pull` (closes [#4957](https://github.com/bobmatnyc/trusty-tools/issues/4957))
  - the default branch is resolved from the repo, never hardcoded to `main`
  - a failed fetch falls back to the last-known remote-tracking ref (or `HEAD`) and logs a warning naming the stale tip, rather than reporting a clean success
  - a repo with no remote still branches from `HEAD`, with no spurious warning
- startup hygiene no longer destroys gitignored working-tree content in the managed checkout (closes [#4961](https://github.com/bobmatnyc/trusty-tools/issues/4961))
  - the update step is now a non-destructive `git merge --ff-only`, never `git reset --hard`, and it refuses when any path the update would write already exists untracked on disk — `git status --porcelain` never reported gitignored paths, so a gitignored file holding real content read as clean and was silently overwritten
  - the update also refuses off the default branch, where the ahead-count validated `origin/<checked-out>` while the operation moved the branch to `origin/<default>`
  - a `.trusty-mpm-no-hygiene` marker file in a base clone now opts that single checkout out of the sweep; previously the only control was the process-wide `TRUSTY_MPM_INPROJECT_HYGIENE` env var
- `tm ls` no longer labels a session with two or more attached tmux clients
  `(active)` instead of `(attached)`. `#{session_attached}` is a client count,
  not a boolean, and the row parser compared it to the literal `"1"` — so the
  session the operator was actually sitting in (two terminals on it) read as
  detached while every single-client session read correctly.
- `tm ls` no longer shows dead session records in its default view (closes [#4994](https://github.com/bobmatnyc/trusty-tools/issues/4994))
  - a record the listing-time sweep confirms dead — no tmux pane, no workspace directory left on disk — was rendered `[dead]` but still listed, because `is_live_session_state` consulted lifecycle state alone
  - visibility keys off that sweep's own verdict, never the raw `unresumable` wire flag: the flag is computed from a record's persisted state and can ship alongside `state: "active"` for a session whose pane is running, which the sweep then refuses to reap. Hidden now implies reapable
  - the records are untouched: `tm ls --all` still lists every one, and the remedy is unchanged (`tm sessions delete <id> --force`)
  - the default-view filter runs AFTER the sweep, so it still sees the records it reaps
  - the "N more dead records pending confirmation" line says where those rows went, and only where they were actually hidden — not on `--json` or `--all`, which print them
  - that line now also counts records whose decommission was attempted and failed. They stay hidden either way, so leaving them out meant a sweep where every attempt failed printed nothing at all and the rows vanished with no explanation
- `tm sessions rename` (and the `tm ls` picker's `r<N> <new-name>`) now surfaces the daemon's own failure message instead of a bare `HTTP status server error (500 …)` (closes [#5001](https://github.com/bobmatnyc/trusty-tools/issues/5001))
  - the client called `error_for_status()`, which discards the response body — the daemon had always sent an actionable reason there, and it was thrown away before the operator ever saw it
  - the daemon also now logs a `warn!` when a rename fails with an unmapped error, so a 500 leaves a diagnosable trace
- `tm ls` no longer staggers `NAME`/`TASK`/`CREATED` on rows whose state carries an annotation
  - the `STATE` column was a hardcoded 14 characters wide, but its rendered value is the state plus any annotation — `attached [stale-assets]` is 23 — so every annotated row pushed the three columns after it nine places right
  - the width is now measured across the rows actually being printed, floored at the historical 14 so an unannotated listing keeps its shape
- Worktree creation no longer logs `worktree index registered` when the trusty-search daemon never confirmed the registration. `index_new_worktree` now reports `RegistrationUnconfirmed` with the reason (unreachable, non-2xx, or suppressed under the test harness) and warns instead of claiming success — [#5045](https://github.com/bobmatnyc/trusty-tools/issues/5045) measured the daemon-absent case at ~94% of worktrees, so this was the common outcome (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- Session launch now decides `skip_vector` from the git-root the index is keyed to, so launching a session in a worktree can no longer re-register that worktree's index with the vector lane on. BM25+KG-only is now an invariant rather than a property of whichever call reached `POST /indexes` first (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- The trusty-search orphan sweep no longer deletes a brand-new worktree's index out from under the walk populating it. `is_orphan_index` gives a `.worktrees` root a 300 s grace window before a `chunk_count == 0` reading counts as abandoned; a root that is gone from disk is still reclaimed at any age (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- `core::worktree_index`'s cost model now states the disk budget beside the 35 s wall-clock figure: colocated stores put ~1–1.5 GiB per worktree on disk, and `skip_vector` removes only the HNSW file, not the corpus (refs [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060))
- `session_context_catchup` returns the paused sessions that actually postdate the watermark ([#5072](https://github.com/bobmatnyc/trusty-tools/issues/5072))
  - the tool returned exactly one session — a hand-written snapshot with every field empty or null — while `resolved_snapshot` pointed at a different, well-formed file. A resume driven off `sessions[]` restored the wrong snapshot; one driven off `resolved_snapshot` restored a file that had never been parsed
  - the response gains `undatable_sessions_dropped`. An empty `sessions` array now only means "nothing paused since your last catch-up" when that count is 0; non-zero means sessions exist but could not be dated and were withheld, and the caller should re-call with `full`
  - `resolved_snapshot` and `sessions[]` still disagree under a recent watermark, by design — they answer "what should I resume from" and "what paused since your last catch-up". What is fixed is the inversion, not the disagreement
  - `recent_commits` / `recent_memory` coming back empty is unchanged and correct — a non-`full` call reports only what is newer than the stored watermark
- `pm_guard` no longer always-allows a bare `MEMORY.md` write, matching the PM prompt's write prohibition ([#5086](https://github.com/bobmatnyc/trusty-tools/issues/5086))
  - `is_pm_orchestration_path` matched any path whose basename was literally `MEMORY.md`, so a top-level `MEMORY.md` landed in the HARD always-allow set that wins over the source-code rule. `core.md` (merged in [#5079](https://github.com/bobmatnyc/trusty-tools/pull/5079)) says never to write, update, or cite `MEMORY.md`, so the enforcement layer contradicted the prompt
  - the two legitimate targets — the palace index under `~/.trusty-tools/…/memory/MEMORY.md` and the retired `.trusty-mpm/MEMORY.md` override — already match on the `.trusty-tools` / `.trusty-mpm` path-component check, so dropping the basename match removes no real coverage. `TASK.md` is unchanged
- Corrected the PM prompt's claim that the enforcement tables cannot be removed by customization. Since #4838 an `ENFORCEMENT` marker in a project's `CLAUDE.md` does replace them; `CORE` is the only structurally protected section. `framework-guaranteed-conventions.md` and `non-overridable-rules.md` now state what "Non-Overridable" governs — the rules are not the PM's to relax, which is separate from whether the section can be replaced — and `assert_authority_intact`'s failure message no longer asserts the false claim.
- `~/.trusty-tools/trusty-mpm/config.yaml`'s `default_model` now actually affects launched sessions ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - the trusty-console Config tab and the `config_write` MCP tool both wrote the field — under a placeholder reading "(unset — uses ~/.trusty-mpm/config.toml)" — and nothing ever read it back, so setting it did nothing. `MpmConfig::load_effective` folds it into `[models] default`, above the TOML value as that placeholder advertises and below the project file
- a misspelled key in `~/.trusty-mpm/config.toml` or `~/.trusty-tools/trusty-mpm/config.yaml` is now reported instead of silently dropped ([#5207](https://github.com/bobmatnyc/trusty-tools/issues/5207))
  - both loaders answer a parse failure by returning `Default`, so putting `deny_unknown_fields` on `MpmConfig` or `TrustyToolsConfig` would upgrade "one key is ignored" into "the whole file is ignored" — a worse failure than the one it fixes. They keep the lenient parse and gain a warning naming every dropped key path (`models.defualt`), derived from a serde round-trip so it needs no hand-maintained key list and cannot drift as sections are added
  - `Project` (`projects.json`) is deliberately left lenient too: it is machine-written, and denying unknown fields on a persisted record would make an older `tm` refuse to read a registry a newer one wrote
- the console's auto-resume toggle now changes what the supervisor does (closes [#5208](https://github.com/bobmatnyc/trusty-tools/issues/5208))
  - `auto_resume_set` wrote `~/.trusty-mpm/auto_resume` and reported success, but the only reader was the console's own status display. `TRUSTY_MPM_AUTO_RESUME`, read once at supervisor startup, was the sole thing that gated a resume, so toggling auto-resume in the console changed nothing in the running fleet and raised no error
  - `Supervisor::tick` re-reads the file every sweep. Precedence is: the persisted file when present > the boot-time `TRUSTY_MPM_AUTO_RESUME` / `--auto-resume` flag > off. An operator's change takes effect within one poll interval, with no process restart
  - the read uses a new tri-state `auto_resume::read_override_at`, which tells "file absent" (leave the boot flag alone) apart from "file says false" (turn a running supervisor off). The existing `read_desired_at` flattens both to `false`, which would have let a never-touched toggle disable an env-enabled supervisor
  - an unreadable desired-state file holds the last known value and logs at `error` instead of failing open to off
  - a failed auto-resume now marks the session `Errored` rather than leaving it `Stopped`. It was previously a `warn!` line and a counter on an endpoint the console does not read, while the session stayed dead and the next sweep silently retried it forever. `Errored` shows up in `FleetMetrics.errored`, which drives the console's Degraded health, and `resume` still accepts `Errored` so a manual retry works
  - `supervisor_status` / `console_metrics` gained an `effective` field (what the next sweep will do) and now report `pending_restart: false` — the "restart pending" hint described the old behavior and would be a false claim now
- retiring a bundled skill now removes its deployed copy instead of orphaning it ([#5224](https://github.com/bobmatnyc/trusty-tools/issues/5224))
  - the skill deployer only ever writes, so a skill the binary stopped shipping kept its deployed directory and its ledger entry forever. Claude Code went on loading text that was deliberately removed, and the orphaned ledger key made `tm doctor`'s `skill_staleness` check report `Unknown` — a check reporting `Unknown` has stopped protecting anything
  - `tm install` and every session launch now sweep all three deploy tiers. A skill is retired only when no live source has it: not the compiled-in bundle, not the resolved bundled source, not `~/.trusty-mpm/skills/`, not the synced catalog, and not the project-custom stems already on disk
  - a user-tier or hand-placed project-tier skill is never removed, and a retired copy the operator edited — or one sharing its directory with a file trusty-mpm never deployed — keeps every file; only the ledger claim is released
  - deselecting a skill via a harness manifest still leaves its deployed copy alone; that remains `tm catalog apply --prune`'s business
- a session in a worktree cut before `CLAUDE.md` was tracked no longer runs on fabricated instructions (closes [#5228](https://github.com/bobmatnyc/trusty-tools/issues/5228))
  - `instruction_pipeline::load_or_create_claude_md` read "file absent" as "new project" and wrote `CLAUDE_MD_STUB`. On a branch cut before #4660/#4661 tracked the real `CLAUDE.md`, that replaced every project instruction — and every `CLAUDE.md` named-section override — with boilerplate the session had no way to detect
  - the damage also persisted: once the stub was on disk, every later run found a file present and took the read path, so the fabrication was indistinguishable from an authored file
  - it now asks git whether a remote-tracking ref already carries the file — the branch's own `@{upstream}` and `origin/HEAD`, falling back to `origin/main`/`origin/master` only when neither of those resolves, so an abandoned `origin/main` cannot override a `develop` default branch. When one carries it, no stub is written and the launch is refused — `prepare_session` maps the error onto `PrepError::Instructions`, the one condition #4752 rules must stop a session
  - the refusal names where the tree sits, the ref that has the file, and the recovery: `git merge --ff-only <ref>` or `git checkout <ref> -- CLAUDE.md` on a branch, and only the second on a detached HEAD, where a fast-forward would move HEAD off the checked-out commit
  - detection is local-only (no fetch) and fails open: no git, not a work tree, or no candidate remote-tracking ref carrying the file all still seed the stub, so a genuinely new project is unaffected. A missing `@{upstream}` alone does not fall open — `origin/HEAD` is still consulted
  - `instructions_failure_message` no longer says "could not write" and no longer points only at permissions and free space — nothing is written on this path, and the refusal carries its own remedy
- `session_context_catchup` no longer hands a session another session's pause snapshot ([#5272](https://github.com/bobmatnyc/trusty-tools/issues/5272))
  - resolution used to fall through to "newest pause overall", then `LATEST-SESSION.txt`, then an mtime scan — all three session-blind. A session with no snapshot of its own got whichever one existed, with nothing in the response saying whose it was. Correct under the one-session-per-checkout model that introduced the chain ([#2731](https://github.com/bobmatnyc/trusty-tools/issues/2731)); wrong now that the PM runs on the project's main checkout and several sessions share one `.trusty-mpm/sessions/` store
  - `resolved_snapshot` is now resolved strictly for the `session_id` you pass. No id, or an id that owns no snapshot, returns null
  - reading another session's state still works — pass that session's id. That explicit request is the only way a cross-session read happens
  - `session_context_pause` writes snapshots to `.trusty-mpm/sessions/<session-id>/`. Existing flat `session-YYYYMMDD-HHMMSS.md` files at the store root are untouched and still resolve, through the `sessions-log.jsonl` line that attributes them; a flat file with no log line resolves for nobody rather than for whoever asks
  - the catch-up digest scans per-session directories as well as the store root, so snapshots written under the new layout still appear in `sessions[]`
  - a `sessions-log.jsonl` entry naming a path outside the store is refused instead of read

### Performance

- bundled agents no longer carry boilerplate `skills:` frontmatter, cutting the skill bodies the harness renders into every dispatch by 78% (2,602,763 → 560,654 bytes across the 37-agent roster) (closes [#4642](https://github.com/bobmatnyc/trusty-tools/issues/4642))
  - worst case `qa` 84,343 → 21,170 bytes (~21,085 → ~5,292 tokens); `research` 55,894 → 0
  - an omitted skill is unchanged on disk and still invokable on demand via the Skill tool
  - a new regression gate caps any bundled agent's resident skill bodies at 24,000 bytes (34,000 for the rust family, which keeps `rust-build-performance` per the standing 2026-07-17 directive)

### Changed

- new sessions provisioned from a repo URL now get their worktree at `<project-root>/.worktrees/<name>`, following the git convention, instead of nesting it inside a `.base` bare clone (closes [#4270](https://github.com/bobmatnyc/trusty-tools/issues/4270))
  - the base checkout is now the project directory's own clone, the same one the in-project spawn path establishes, so both paths share one base clone per project
  - existing `.base` stores are left exactly as they are — still discoverable via `git worktree list`, still working — and provisioning refuses loudly rather than moving or deleting one; retiring `.base` stays a manual operator step
  - a project directory holding git or trusty-mpm state (`.git`, `.base`, or `.worktrees`) is never offered for `mv`-aside recovery and never renamed by the old-layout migrator; the presence probe treats an unreadable entry as present, so a permissions blip cannot read as "no git dir"
  - the base clone's `.worktrees/` is excluded in `.git/info/exclude`, as the in-project path already does — without it `git clean -ffd` in the base deletes every session worktree including uncommitted work (single-force `clean` skips them; `-ff` does not)
- the composed PM prompt drops 6,611 bytes (56,783 → 50,172, −11.6%) by relocating content the PM cannot act on and deduplicating the voice rules ([#4574](https://github.com/bobmatnyc/trusty-tools/issues/4574))
  - `Skill Deployment` / `Agent Deployment` / `Skills System` collapse to one dispatch-time block; the tier tables were already generated and drift-gated in `tm-capabilities` (`references/framework.md`), and the deployment lifecycle they also carried moves to that skill's hand-authored `references/workflows.md`
  - the `gh label create` / `gh issue create` shell block leaves the prompt — `tm-ticketing` and `tm-pr-workflow` already carry it verbatim, and P6/P7 forbid the PM running it
  - the Fail-Open Check's five review steps move to the `code-review-standards` skill, which `code-critic` already loaded and `code-analyzer` now declares; the BLOCKING rule and its error-arm-test requirement stay in the prompt
  - `Prose Style — Write Plainly` becomes a pointer: the voice rules are stated once, in the output style, which was carrying a live mirror of the same text. Both copies were session-resident, so the project paid for them twice
  - dead rules dropped: `/mpm-configure --preview` and `/mpm-init` name no command that exists
  - `Delegation Mechanics` said bundled agents deploy to `~/.claude/agents/`; they deploy to `$CLAUDE_CONFIG_DIR/agents/`, which tm never writes into the operator's own install
- the PM voice rules gain two entries, in the output styles and in `BASE-AGENT.md`: don't justify the restraint ("I don't know yet" is the whole answer), and no trailing emphatic negation ("— not before" restates the sentence by negating its opposite)
- the composed PM prompt drops 18,947 bytes (52,921 → 33,974, −35.8%; ~20.4k → ~13.1k tokens at 2.6 chars/token) by applying the owner's per-prompt rule — an instruction not needed on EVERY prompt lives in a skill and keeps a one-line trigger here ([#4595](https://github.com/bobmatnyc/trusty-tools/issues/4595))
  - moved to `tm-delegation-patterns`: the retry protocol, task-complexity sizing, the batching anti-pattern table, per-agent model overrides and the cost model, the `code-critic` dispatch standard, `isolation: "worktree"`, cross-workstream claim drawers, the structural delegation brief, and the architecture-suggestion cap
  - moved to `tm-workflow`: the per-phase dispatch-brief templates, the override commands, and the close-and-fold / branch-vs-worktree half of the sprint-then-harden doctrine
  - moved to `tm-pr-workflow`: the pre-push credential scan, which the PM delegates rather than runs
  - moved to `tm-circuit-breaker`: the Quick Violation Detection list, which restated the two tables above it
  - reduced to pointers, with the imperative left resident: the customization mechanics, the Trusty tool-priority per-tool tables, the direct-action budget's `pm_guard` detail, and the `Fail-Open Check`'s five steps
  - the framework floor keeps every imperative — the Prohibitions table, the Circuit Breakers table, the direct-action budget with both its halves, and the Framework-Guaranteed Conventions
- the PM asks the user based on observable conditions (ambiguous requirements, a missing credential, an irreversible architecture choice, an unrequested destructive step) rather than an uncalibrated "<90% success probability" estimate
- the four-part report template applies to task-completion reports only, not to every PM response
- Moved the customization doctrine into `core.md`, the only section a project's `CLAUDE.md` cannot override. One surface per artifact type: prompt/instruction sections are customized in the project's root `CLAUDE.md` via named-section markers and nowhere else; skills through the skill tiers (project `.claude/skills/` > user `~/.trusty-mpm/skills/` > bundled, a hand-edited deployed skill frozen against redeploy by design). Ad-hoc channels — the retired `.trusty-mpm/` files and anything shaped like them — stay banned as a third channel duplicating the first. Second half: what earns a place in `CLAUDE.md` is what is needed on EVERY prompt, since it is resident every turn; content needed only sometimes belongs in a skill, a doc under `docs/`, or memory. Stated in the framework-floor section instead, as it was, a project could override away the rule telling it not to override elsewhere (the hole [#4722](https://github.com/bobmatnyc/trusty-tools/issues/4722) describes). `Customizing PM Behavior` keeps the mechanics — marker syntax, the section-token table, `tm sessions instructions` verification — and now points at CORE for the rule
- the compiled PM prompt is now written per project, to `<project>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`, and a failure to write it refuses the launch (refs [#4752](https://github.com/bobmatnyc/trusty-tools/issues/4752))
  - removes the cross-project collision by construction: concurrent sessions no longer share one global file
  - the resume path writes it too, so resumed sessions can no longer run against a prompt that was never recorded
  - `tm install` no longer writes a compiled prompt — install has no project, so it has nothing to compile
  - a refused launch names the path and the remedy instead of surfacing a bare I/O error
- the prose rules in `sections/core.md` ("Write Plainly" and clickable references) now govern every artifact the PM authors — dispatch briefs and ticket/PR body text, not only its own responses — and add the six concrete-referent/cause-effect/before-after rules the owner ratified for framework prose (closes [#4757](https://github.com/bobmatnyc/trusty-tools/issues/4757))
- framework engineers now gate on framework evidence instead of language
  evidence ([#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - `react-engineer`, `nextjs-engineer`, and `svelte-engineer` no longer deploy
    to a JavaScript project that does not declare the framework as a
    dependency; `phoenix-engineer` no longer deploys to a non-Phoenix Elixir
    project, which `elixir-engineer` now covers instead
  - `gcp-ops` and `vercel-ops` are platform-gated and no longer deploy to
    projects with no GCP or Vercel marker
  - project marker matching gained a bounded content-probe form
    (`<path>::<needle>`) for React and Phoenix, which ship no config file that
    distinguishes them from plain JavaScript and plain Elixir
  - markers are now evaluated at the project root AND at every workspace member
    the root manifest declares (npm/yarn `workspaces`, `pnpm-workspace.yaml`,
    Elixir umbrella `apps_path:`), so a monorepo that keeps its framework in a
    member package keeps its framework engineers; the walk is bounded to one
    glob level, 256 members, and 16 MiB per detection call, and fails closed
- `framework-manifest.toml` is now the single authority for agent AND skill
  bundling ([#4765](https://github.com/bobmatnyc/trusty-tools/issues/4765))
  - each gated agent declares its own `markers` in the manifest; the
    `LANGUAGE_ENGINEERS` and `PLATFORM_AGENTS` Rust tables are gone, so an
    entry's category and the condition that deploys it are one declaration
  - a new `[skill_categories]` section declares the bundled skill roster; a
    bundled skill nobody declares is a hard error, matching the agent side
  - a gated entry declaring no markers is rejected rather than silently
    becoming undeployable
  - stack and platform detection now share one probe budget and one
    workspace-member resolution per call instead of one of each
  - `references/agents.md` gains a **Deploys When** column rendered from the
    manifest, and `references/skills.md` renders the declared skill roster
  - the bundled `tm`, `tm-delegation-patterns`, and `agent-delegation`
    documents no longer restate the roster or its gates — they point at the
    manifest and at the generated reference
- Retired the blocking-CI-wait doctrine across every bundled instruction copy: agents now push, take a ONE-SHOT `gh pr view` / `gh pr checks` status read, report, and end their turn; the PM owns re-engagement when CI settles ([#4792](https://github.com/bobmatnyc/trusty-tools/issues/4792))
  - `gh pr checks --watch` is now forbidden — it streams check output into the agent's context (546k tokens over 54 minutes on one PR). The retirement is about context cost, not runnability
  - `BASE-AGENT.md` "Foreground Execution — NEVER End Your Turn To Wait" is replaced by "Finishing Work — Push, Report, Stop"; own-gate commands still block in the foreground
  - `version-control.md`, `local-ops.md`, `BASE-ENGINEER.md`, `tm-delegation-patterns.md`, and the `trusty-code` asset mirror updated to match
  - PM instructions section renamed "Parked-Subagent Re-Engagement": a hand-back with CI pending is correct behavior, not a park, and must not be nudged back into a blocking wait
  - `idle_nudge::DEFAULT_NUDGE_MESSAGE` no longer tells a stalled pane to run `gh pr checks --watch`
  - The one-shot read now documents two traps: `bucket` can report a false DONE under GitHub API eventual-consistency lag (cross-check `state`), and repeated `gh pr update-branch` is a treadmill that mints a new untested head each time (BEHIND is not a correctness gate)
- `.trusty-mpm/` now belongs to a project's MAIN CHECKOUT, never to a worktree (closes [#4832](https://github.com/bobmatnyc/trusty-tools/issues/4832))
  - the compiled PM prompt moved to `<project>/.trusty-mpm/sessions/<session-id>/INSTRUCTIONS-COMPILED.md`; two concurrent MANAGED sessions in one project can no longer overwrite each other's. Launches with no session identity (in-place start, standalone load, `tm connect`) share a `sessions/local/` bucket and can still collide there — narrower than the single per-project file it replaces, not collision-free
  - the project `manifest.toml` override moved to `<project>/.trusty-mpm/framework/manifest.toml`
  - the `last-instructions.md` stash and `tm project init`'s scaffolding resolve against the same root, so a session launched from a worktree writes no `.trusty-mpm/` into it
  - the root is resolved from git: a linked worktree maps to its main checkout and trusty-mpm's own bare `<project>/.base` provisioning clone maps to the project, while a submodule, a `--separate-git-dir` checkout and an ordinary repository merely *named* `.base` each own their own state
- **Removed feature (owner ruling, not a bug fix):** the user-level `~/.trusty-mpm/manifest.toml` layer is gone. Instructions and their manifest are per-project and always deployed locally, so there is no user-level manifest surface. Move any home-level overrides into `<project>/.trusty-mpm/framework/manifest.toml`.
- migration is automatic and best-effort: every launch deletes a pre-existing `<dir>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md` (in the worktree and at the harness root) and removes the `framework/` directory only when it actually migrated a file and the directory is left empty, so neither an operator's `manifest.toml` nor a `tm project init` scaffold is disturbed
- a pre-existing `<project>/.trusty-mpm/manifest.toml` is still deliberately not moved for you, but a launch that finds one with no manifest at the new path now warns and names both paths, so an override cannot go dark silently
- the PM instruction package now carries a binding rule, in the non-overridable `core` section, that `MEMORY.md` (or any static memory-index file) is never a write target or a source — durable facts go to the palace instead, and `CLAUDE.md` is the only non-dynamic instruction source (closes [#4834](https://github.com/bobmatnyc/trusty-tools/issues/4834))
  - the PM Allowlist's "write a single non-source file" row no longer lists a bare memory file as an unbudgeted write
  - `sections/memory.md` carries a one-line pointer back to the new rule
- `BASE-AGENT.md` "Never Directly Monitor a Declarative Process" no longer recommends `<command> 2>&1 | tm compress`, which took its exit code from `tm compress` and masked a failing gate. The trim now reads from the captured file (`tm compress --tool "cargo test" < /tmp/gate.txt`) and only when the verdict is non-zero
- Collapsed six agent-routing surfaces in the compiled PM prompt into one Routing Table plus the generated roster. `Agent Routing` (core), `When to Delegate to Each Agent`, `Ops Agent Routing`, `Make / Mise Command Routing` and `Common User Request Routing` all answered "which agent handles what" for the same ~11 agents; every routing fact that existed only in a deleted table — `make`/`mise run` delegation, version/publish → `local-ops`, the browser-tool ban, the "just do it" pipeline — was folded into the survivor and is pinned by `the_surviving_routing_table_covers_every_folded_mapping`
- Dropped the `Handles:` line from generated Delegation Authority roster entries. Its text was the agent's frontmatter `description`, byte-identical to the description the harness already publishes in its own Agent-type catalog; `Role:` and `Model:`, which the harness does not supply, are kept
- Stated the direct-action budget once, in `enforcement.md`'s canonical "The direct-action budget (P1 and P5 only)". `core.md`, `identity.md` and `non-overridable-rules.md` now point at it by title instead of restating it — the floor stays self-sufficient because `enforcement.md` is itself part of the floor
- Rewrote `BASE-AGENT.md` in a compressed, instruction-dense register for its agent reader: imperative mood, tables over prose, rules stated once, examples kept only where the rule alone is ambiguous. All 139 rule markers verified present before and after
- Compiled PM prompt is 12.2% smaller — 66,628 → 58,564 bytes, roughly 2,180 fewer tokens on every PM session launch
- The daemon and supervisor launchd labels used by the autostart gate, the
  launchd probe, the unsupervised-daemon refusal, and the MCP bridge's no-spawn
  hint now come from `trusty_common::launchd_labels` instead of four separate
  literals. Values are unchanged. The hints matter most: a remedy string naming
  a plist the host does not have is exactly #2827, and #1900 was itself a
  label-lookup miss (#4868)
- `tm register` now takes the repo first with an optional alias, and `owner/repo` is the primary form (closes [#4912](https://github.com/bobmatnyc/trusty-tools/issues/4912))
  - `tm register bobmatnyc/trusty-tools` registers `https://github.com/bobmatnyc/trusty-tools` under alias `bobmatnyc-trusty-tools`. GitHub is assumed, matching the `is_github_remote` gate `tm launch` already applies
  - a full URL is the alternative form and any host works there — `https://…`, `git@host:owner/repo.git`, `.git` suffix, trailing slash, ports. Non-GitHub *shorthand* is deferred, not refused on the merits
  - with no alias, one is derived as hyphen-joined `owner-repo`; the hyphen is deliberate, since an alias becomes a path segment wherever it is consumed
  - the legacy `tm register <alias> <url>` order still works: whichever positional names a repo is taken as the repo
  - a URL with no owner segment falls back to the bare repo slug; a derived alias already bound to a different URL refuses without touching the registry
  - **behaviour change:** arguments that name no repository are now refused instead of registered. A host with no path (`https://example.com`), browser paths into a repo (`…/tree/main`, `…/pull/123`), relative paths (`./repo`, `../repo`, and dot-prefixed directories like `.hidden/repo`), and bare words are all errors naming the accepted forms. Each previously registered silently under a wrong alias. A `?query` or `#fragment` is stripped before the URL is stored
  - **behaviour change:** a GitHub URL with any path past `owner/repo` is refused, so single-word tab URLs (`…/owner/repo/issues`, `/pulls`, `/releases`, `/wiki`) are caught too. They used to register `repo-issues → …/owner/repo/issues` at exit 0 — unclonable, and the wrong alias never collided with the canonical `owner-repo`. Hosts that nest legitimately are unaffected: `gitlab.com/group/sub/blob` still registers, as does a repo actually named `tree`
  - **behaviour change:** a URL is now stored in a form `git clone` accepts. A scheme-less host gains `https://` (`github.com/owner/repo` → `https://github.com/owner/repo`), and a leading `~/` is expanded (`~/src/repo.git` → `/home/you/src/repo.git`). Both used to be stored verbatim and failed at `tm load` time with `repository … does not exist`
- `tm-pr-workflow`'s frontmatter description now names the per-PR changelog fragment requirement, so a relevance match can load the skill instead of only an explicit pointer reaching it ([#4968](https://github.com/bobmatnyc/trusty-tools/pull/4968))
  - the description previously covered only branch protection, the trusty-review gate, squash-merge, and worktree discipline, so an agent searching for the `changelog.d` fragment format never matched the one skill that documents it
  - `tm-capabilities`' generated skill catalog is regenerated to match
- PM prompt: moved instructions not needed on every prompt into the skills that already held them, shrinking the compiled prompt 60,243 → 56,783 bytes (−5.7%) ([#4969](https://github.com/bobmatnyc/trusty-tools/pull/4969))
  - the QA evidence table, QA-target routing table and forbidden-phrase list are stated once, in `tm-verification-protocols`; `core` keeps the gate trigger and `workflow`'s third copy is gone
  - PM identity is stated once in `core`; the `identity` section keeps only its own tm-session context
  - the attribution footer is stated once in Framework-Guaranteed Conventions; `workflow`'s inline copy is gone
  - parked-subagent re-engagement MECHANICS moved to `tm-delegation-patterns`' new "PM Re-Engagement" section, whose frontmatter description now names the CI-wait / parked-agent trigger so it can fire on its own; `core` keeps the trigger
  - every skill pointer now reads as an instruction naming the `Skill(...)` call. Bare `[SKILL: name]` notation is documentation style, not a tool call, which is how a pointer and a re-inlined copy of its target came to sit side by side; a test forbids the notation from returning
- bundled output styles now set `keep-coding-instructions: true` and carry the PM prose rules directly
  - the field defaults to `false`, so all three styles were silently stripping Claude Code's built-in scoping, comment, and verification instructions
  - the `Prose Style — Write Plainly` rules are mirrored from `assets/instructions/sections/core.md` into each style, on the same #2647 rationale as the PRIMARY DIRECTIVE — the output style is the only channel that survives a manual `claude` launch with no tm-appended system prompt
  - the older, lighter `## Communication` block is folded into the mirrored rules rather than left beside them
  - `BASE-AGENT.md` gains a graduated-verbosity rule: sparse on a clean pass, detailed on failures — the evidence rule (raw output for failures, flakes, and performance claims) is unchanged
- `tm ls` now evicts session records that have sat in a terminal state
  (`decommissioned`/`deleted`) for more than 7 days, and releases the `NUM`
  slot each evicted record held. Numbers stopped tracking the visible fleet
  because nothing ever left the store — 76 of 107 slots on one machine were
  held by rows the default view hides, which is why `NUM 107` appeared at the
  bottom of a 31-row listing. Retention runs on the daemon's existing GC tick.
  A record whose workspace directory still exists on disk is never evicted.
  Records written before this change carry no terminal timestamp; they are
  backfilled from their last evidence of life rather than from the current
  time, so an already-stale backlog is cleared on the next sweep instead of
  seven days after upgrading.
- `tm ls` colors the `NUM` and `NAME` columns in two distinct hues (magenta and
  cyan). Deliberately not one hue: a name's trailing serial is per-project and
  reuses gaps, while `NUM` is a global slot, so any match between them is
  coincidence. Color is gated on stdout being a TTY and honours `NO_COLOR`;
  piped and redirected output is byte-identical to before.
- `ticketing` agent must search open AND closed issues and reopen a closed ticket for a recurrence before creating a new one, and must report which outcome it took
  - ticket bodies are now a rule, not advice: defect, evidence, resolution — no structured headings, no restating linked material, file and symbol instead of line numbers
  - `tm-ticketing`'s issue schema is relabelled six facts a reader must be able to tell, not six headings to fill in
- `ticketing` agent must label every issue at creation with three families — one type (`bug`/`enhancement`/`refactor`/`chore`/`documentation`/`epic`), one or more component/crate labels, and `P0`–`P3` only when the issue text itself asserts severity — on top of the existing `--assignee @me` / `trusty-mpm` / `ws/<session>` defaults
  - milestones are release slots, not a field every issue receives: left unset unless the issue is deliberately scheduled into a release confirmed open, and never used to hold a `ws/` workstream value
  - the agent's report now names the labels applied and the milestone state for anything it created
  - `tm-ticketing` states that the two harness defaults are not the whole label set and points at the agent asset for the rest
- PM instruction package trimmed from 31,940 bytes / 4,764 words to 27,921 bytes / 4,179 words (-12.6% bytes, -12.3% words). The per-agent routing table and default-model column moved to the `tm-delegation-patterns` skill, which loads on trigger; `workflow.md`'s duplicate 5-phase table and `non-overridable-rules.md`'s duplicate marker-block mechanics were replaced by pointers; the Prohibitions and Circuit Breakers tables were compressed. No prohibition, circuit-breaker code, routing target, or the direct-action budget was removed. The #4595 word target was revised 2,000-3,000 -> ~4,000 on #5087: ~4,000 is the measured floor that keeps every rule.
- Workflow, ticketing, and version-control policy now has one owner per surface, and `tm-pr-workflow` is retired into `tm-workflow` ([#5202](https://github.com/bobmatnyc/trusty-tools/issues/5202)). `tm-workflow` is the sole workflow skill: it carries the delivery chain, one-outcome-one-PR, the phase dispatch briefs, worktree and branch discipline, the pre-push credential scan, branch protection, the changelog gate, the seven-field PR body, the baseline-failure protocol, the trusty-review gate, squash-merge, and the `CLAUDE.md` customization mechanism. `tm-ticketing` is the sole issue authority: promotion, deduplication, ticket style, labels, milestones, lifecycle, and issue attribution. The composed core instructions now route to exactly those two and state no workflow policy of their own.
- Routing between `ticketing` and `version-control` is now by ARTIFACT, not by a bookkeeping/mechanics distinction. Every Issue operation is ticketing's; every Pull Request operation — **including the PR title and body, first draft and every later edit** — plus every git operation is version control's. Previously core, `tm-ticketing`, and the `ticketing` agent called PR-body authorship "ticketing bookkeeping" while P7, `tm-pr-workflow`, and the `version-control` agent assigned the PR to version control, which split a single `gh pr edit` across two owners. P6 and P7 now describe the whole artifact rather than a partial verb list.
- The ticketing→workflow→version-control→ticketing handoff is documented once, in `tm-workflow`, and referenced from core. Neither specialist delegates to the other; the PM carries the canonical issue ID, outcome, and closure conditions into the version-control brief, and hands the merged PR and squash SHA back to ticketing for closure.
- Deduplication now produces one of four named dispositions — `COMMENT`, `REOPEN`, `NEW REGRESSION`, or `NO TICKET` — each with its own criterion, replacing the previous unconditional "reopen on any recurrence". A verified fix followed by a different failure mode is new work, not a reopen.
- Ticket bodies now require a type-aware title and one to four observable closure conditions on top of the existing sparse-body rule, with bounded length exceptions for epics, security issues, and research/audit issues only.
- Labels are stated as four separable families (type, owning component, optional triaged priority, optional provenance) and no instruction may invent a label the repository does not carry. `tm-bug-reporting` previously required `agent-error`, `skill-error`, and `high-priority`, none of which exist in this repository; it now defers to `tm-ticketing`'s families and sparse-body form instead of its own five-heading template.
- Milestones stay unset unless the issue is deliberately scheduled into a confirmed-open release, is child work a release-gating parent carries, or is a blocker for a release already in flight.
- Conflicting ticket prose is reduced to a pointer in all three output styles, `BASE-AGENT.md`, `tm-bug-reporting`, and `SM_WORKFLOW.md`. The output styles previously required file+line citations while the `ticketing` agent forbade line numbers; the styles now govern voice only and defer the binding schema to `tm-ticketing` and `tm-workflow`.
- Ticketing model guidance in `tm-delegation-patterns` now says `sonnet`, matching the embedded agent's frontmatter and the generated roster; it previously said `haiku`.
- `tm doctor`'s `skill_staleness` conventions-bearing allowlist now names `tm-workflow` in place of the retired `tm-pr-workflow`, so the attribution footer, the shipped label/assignee defaults, and squash-merge-only still escalate drift to `Fail`.
- The PM's resident `## Session Management` section now names the session-lifecycle verbs directly — `tm session ls | rename | pause | resume | stop`, with `rename`'s one-arg in-session form — instead of pointing only at `tm-session-management`, which covers context-limit pause/resume snapshots and never mentioned the CLI verbs. A PM asked to rename, stop, or list a session answers from resident instructions rather than dispatching an agent to discover the command (#5211). Paid for by cutting redundant resident text: two back-pointer stubs carrying no rule (`identity.md`'s "stated once in CORE", `memory.md`'s "see Core's Memory & Instruction Sources"), the second copy of the retired `.trusty-mpm/` override-file list, and the third restatement of what "Non-Overridable" means. Package: 4,268 -> 4,231 words.
- `tm-cli-operations` documents `tm session rename` in both forms; it was the one lifecycle verb the skill's session listing omitted, present only in the generated `tm-capabilities` CLI reference.
- A session now starts in the project's main checkout instead of provisioning
  its own git worktree. The project's `worktree` setting no longer decides this
  — it decides whether the AGENTS a session dispatches get isolated, and a
  project registered `worktree: true` still isolates them exactly as before.

### Removed

- the deprecated `ops` agent is deleted from the bundle; it was superseded by
  `local-ops` and still reached every roster
  ([#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - an `ops.md` already deployed to a machine is NOT retracted — orphan
    retraction is [#391](https://github.com/bobmatnyc/trusty-tools/issues/391)
    and has not shipped
  - trusty-code's embedded mirror of the agent catalog drops `ops.md` and gains
    `elixir-engineer.md`, keeping `scripts/check_agent_assets.sh` green
  - the bundled `tm` skill and `agent-delegation.md` no longer name `ops` in
    their rosters, and now say which agents are marker-gated rather than
    implying every bundled agent reaches every project
- `PipelineOutput::merged` and `PipelineOutput::instructions_loaded`, and `PipelineInput::framework_instructions_path`. All three existed only to serve the retired framework-instructions read and had no production consumer

### Security

- auto-prune can no longer destroy anything outside the session registry: `decommission_record_only` is now a dedicated single-effect function rather than a flag on the destructive teardown (closes [#4728](https://github.com/bobmatnyc/trusty-tools/issues/4728))
  - as a flag it left two destructive effects ungated — a runtime SIGTERM plus `kill_session` against any live pane sharing the record's name, and a cross-daemon `DELETE /indexes/{id}` whose target resolved to the PARENT PROJECT's search index once the workspace was gone
  - both were reachable from `tm ls` in 1.3.3 and 1.3.4; neither required a name collision
  - `decommission_with_root` keeps every effect, no longer takes a `record_only` parameter, and now documents its side effects as an explicit table
- allowlisted untracked files (default `.env*`) are no longer copied into a session worktree until git confirms they will be ignored there (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - the `info/exclude` registration ran AFTER the copy and a failure was only a `warn!`, so a worktree whose `git rev-parse` merely failed — a stale gitlink, `detected dubious ownership`, an unreadable `.git` — was left holding the operator's `.env` unregistered, where a later `git add -A && git commit` stages it into history
  - the order is inverted: register first, then re-verify each path with `git check-ignore` (git's own authority, mirroring `native_mcp::is_env_local_actually_ignored`) and copy only what it confirms
  - this also stops a secret overwriting a path the repo already TRACKS — git reports tracked paths as not-ignored no matter what `info/exclude` says, and `git add -A` would stage the change
  - a destination with a corroborated absence of any repository has no history to leak into and is still copied to freely, so non-git destinations are unaffected
- agent-cost persistence hatch: close the git option-abbreviation bypass ([#4850](https://github.com/bobmatnyc/trusty-tools/pull/4850))
  - git's `parse-options` resolves any unambiguous prefix of a long option, so `--exe` is `--exec` and `--rece` is `--receive-pack`. The exec-option deny list matched names exactly, so `git push --exe=<program> …` and `--rece=<program>` classified as "persistence only" past the context ceiling and ran the named program. The post-subcommand long-option surface is now default-deny against a safe allowlist, so an abbreviation of a dangerous option — and any construct nobody has thought of — is rejected without being enumerated.
  - the program check compared the basename, so `./git`, `/tmp/evil/git`, and `../../tmp/evil/git` all passed as "git". It now requires the literal token `git`; a path-qualified `git` is rejected.
  - the deny text an agent sees at the ceiling now names the shell-metacharacter rule and the single-quote fix, so `git commit -m "fix $ISSUE"` no longer denies without a hint. It also names the `::` rule and that quoting does not exempt it, so `git commit -m 'use std::process::Command'` — routine content in a Rust workspace — denies with a reason instead of silently. Narrowing that rule to spare quoted `-m` values is tracked in [#4869](https://github.com/bobmatnyc/trusty-tools/issues/4869).
  - corrected the module's residual note: `-C` / `--git-dir` are allowed globals, so the repository whose config supplies `diff.external` is attacker-choosable in the same command, not confined to the agent's own repo. The note now also names git hooks (a planted `.git/hooks/pre-commit` fires on `git commit -am hooked` while the classifier allows the command) and `core.fsmonitor`, which fires on a bare `git -C <path> status`.

### Documentation

- corrected trusty-mpm's stated skill precedence, which had Claude Code's runtime order backwards and conflated it with tm's deploy-time order
  - `mpm-skills-manager` and `mpm-agent-manager` now carry the verified runtime resolution for their own artifact type and state the asymmetry: for skills personal `~/.claude/skills/` beats project, for agents project `.claude/agents/` beats user. Each says where to write a file so it actually wins the collision
  - the skills manager's existing tier section is relabelled deploy-time source precedence, so tm's `project-custom > user-custom > bundled` rule is no longer read as Claude Code's runtime order
  - `core::project_skill_tier` and `core::managed_config` asserted that `<project>/.claude/skills` outranks `$CLAUDE_CONFIG_DIR/skills`. That is backwards for skills, and it was load-bearing for the #4880 redeploy rationale. Both now state the real order and name which axis they mean; the redeploy still earns its place because the project tier is what loads for every name the managed roster does not carry
- the bundled `tm-workflow` skill now carries the full generic worktree discipline, so a project's `CLAUDE.md` no longer has to restate it ([#5269](https://github.com/bobmatnyc/trusty-tools/pull/5269))
  - moved in from this repo's `CLAUDE.md`: branch off `origin/main` never local `main`, "experiments stay session-local", the stash-first escape hatch for a one-off main-checkout command, `git push origin --delete` as the manual cleanup case, QA agents getting their own worktree, and the note that worktree cleanup never touches the main checkout
  - `tm-cli-operations`' `prune-worktrees` line keeps its `.worktrees/` path — that is `tm`'s own session provisioning, which still writes there — and now says so, so it is not mistaken for the canonical `.claude/worktrees/` home

## [1.3.4] — 2026-08-03

### Added

- New bundled `## Source Citations` section in
  `sections/workflow.md`: source citations in docs and reports link to a
  GitHub blob permalink pinned to a commit SHA (never `blob/main`), with
  `path:line` as the link text and the line number verified before linking.
  This is framework doctrine, not project-specific, so it ships in the
  bundled instructions rather than a project-level `.trusty-mpm/` override
  (see [#4578](https://github.com/bobmatnyc/trusty-tools/issues/4578)).

### Fixed

- A changelog fragment carrying a second category is now rejected instead of
  silently mis-rendered. Only line 1 is a category and everything after it is
  copied through verbatim, so bare `Changed`/`Added`/`Fixed` lines became body
  text and every bullet landed under the line-1 heading — the 1.3.3
  `4286-retire-trusty-mpm-override-files.md` fragment put all four of its
  categories under `### Removed`, caught only by a human diffing the `--stdout`
  preview. Both the bare-word and the markdown-heading form (`## Changed`) are
  caught, and `scripts/assemble-changelog.sh` names the file and the exact line
  of each. Fenced code blocks are exempt, so a fragment may document the fragment
  format or show example output without tripping the check. The CI gate inherits
  all of it, because `scripts/check_changelog_fragment.sh` asks the assembler
  rather than re-implementing validation. Guarded by
  `scripts/assemble_changelog_selftest.sh`, which replays the real fragment.

## [1.3.3] — 2026-08-03

### Added

- `tm doctor` check `legacy_overrides`: FAILS when a project still carries any
  retired override file, naming every file found and the migration. The prompt
  resolver logs the same signal on every session launch, so a leftover file can
  never drop a project's rules silently (#4286).
- `crates/trusty-mpm/src/assets/instructions/sections/README.md` documenting how
  framework instructions compose, the customization tiers, why there is no
  floor, and what must not be reintroduced (#4286).

### Fixed

- The seeded `CLAUDE.md` stub could declare a live override. A worked marker
  example in the stub was parsed as a real `WORKFLOW` block — marker recognition
  is whole-line and knows nothing about code fences — so every newly seeded
  project silently lost its entire bundled workflow section and received the
  placeholder prose instead. Found by running a real `tm` instance during
  acceptance; guarded by `seeded_claude_md_declares_no_overrides` (#4286).
- Swept shipped asset text that still advertised the five retired
  `.trusty-mpm/` per-file PM instruction overrides (#4286): the `tm-workflow`
  skill described them as the live customization mechanism and told the PM to
  write to them; three output styles (`trusty-mpm`, `trusty-mpm-research`,
  `trusty-mpm-teacher`) described the compiled prompt as four monolithic files
  (`PM_INSTRUCTIONS.md` + `WORKFLOW.md` + `AGENT_DELEGATION.md` +
  `BASE_PM.md`) that haven't existed since #4183; `tm-delegation-patterns`,
  `tm-circuit-breaker`, and `tm-pr-workflow` pointed at `AGENT_DELEGATION.md`
  / `.trusty-mpm/INSTRUCTIONS.md` as if still reachable. All now describe the
  current model: framework instructions compose from
  `assets/instructions/sections/*.md`, and the sole project-customization
  channel is a named-section marker in the project's root `CLAUDE.md` — `core`
  is the only section such a marker cannot replace.
- `assets/instructions/sections/workflow.md` pointed the project test-ladder
  lookup at the retired `.trusty-mpm/INSTRUCTIONS.md`; it now points at
  `CLAUDE.md`. This is a compiled-prompt change, so the `pm-prompt-bundled-
  fallback.md` and `pm-prompt-roster-absent.md` golden fixtures were
  regenerated (`UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`) to match.
- Verified no scaffolding path (`tm-init`, `tm project init`) creates any of
  the five retired override files — the only writers found were the
  `legacy_overrides` doctor check's own fixtures.

### Changed

- `ticketing` agent's default model tier is now `sonnet`, up from `haiku`.
  Duplicate-detection and scope-boundary judgement (is this issue already
  filed, is this work in scope for a milestone) are judgement calls, not
  clerical ones — observed 2026-08-03: a haiku-tier ticketing agent filed a
  duplicate issue after being told not to, and cleared milestones on a shipped
  release when asked only to report them.
- `core` is now the ONLY section a named-section override cannot replace.
  `identity`, `enforcement`, `non-overridable-rules` and
  `framework-guaranteed-conventions` become tier `project` and are overridable
  like every other section; no content moved between sections. `validate`
  enforces the tier assignment as an iff, so both retiering `core` away from
  `fixed` and marking a second section `fixed` are hard errors (#4286).
- The seeded project `CLAUDE.md` stub now documents the marker grammar, lists
  the accepted tokens, and points at `.trusty-mpm/last-instructions.md` as the
  record of what the session actually received. It no longer refers to
  `BASE_PM.md`, which has not existed since #4183 (#4286).
- Credential resolution now imports from `trusty_common::credentials` instead of
  `trusty_common::inference::credentials`, which was deprecated in the same
  change (see [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564)).
  Import-path churn only — no behaviour, precedence, or credential surface
  changes in this crate.
- The bundled `tm-ticketing` skill now carries a Ticket-Promotion Gate: a finding is not automatically a ticket. Filing requires a duplicate search first (by test name, error text, affected symbol, and package) and then at least one of five promotion criteria — a reproduced user-visible defect, accepted feature work, a different owner/release/dependency/security disposition, work that cannot fit the current PR without changing its outcome or risk, or an explicit user request to track it. Everything else stays a session task, a PR review comment, or a parent-issue checklist item, and "follow-up" is named as a category that does NOT bypass the gate. Four overlapping recommendations are merged into one five-step protocol rather than stacked as near-duplicates: the gate also fixes issue granularity to outcomes instead of findings (one canonical issue per recurring flaky test, occurrences appended; never a separate issue for the tests, docs, changelog, or review cleanup of one outcome) and supplies a six-field issue schema. It cross-references the already-shipped Opportunistic Fixes rule instead of restating it — an easy fix found while working a file never enters the gate at all (#4630).
- Findings filed as issues now state a confidence level — Observed, Reproduced, Inferred, or Speculative — with a default disposition for each. Inferred and Speculative belong on the parent issue or PR unless severity justifies escalation. The skill says explicitly that nothing reads this label mechanically: it is a drafting rule whose only check is whether a reader of the filed issue can tell which state was claimed (#4630).
- The bundled `tm-pr-workflow` skill now states the one-outcome-one-PR invariant, which the skill previously lacked entirely. A PR carries one primary outcome plus everything needed to ship it safely — implementation, regression tests, refactoring, docs, the changelog fragment, and in-scope review fixes — and those are never split across PRs merely because different agents produced them. One PR may close several tickets when one coherent change satisfies them, which is preferred over coupled PRs with an artificial merge order. A seven-field minimal PR body accompanies it (#4630).
- `tm-pr-workflow` gains a six-step baseline-failure protocol for a red gate the branch did not cause: establish causation against the base branch, fix branch-caused red in the PR, append run/SHA/command/failure-signature to the canonical issue when the failure is already tracked, and open exactly one canonical issue when it is not — after reproduction or sufficient CI evidence, never from a single unrelated red run. Step 6 mandates a literal report string, `change-specific gates pass; <gate name> blocked by canonical issue #N`, so the rule is checkable; "all tests pass" is never reportable while a gate is red. Step 1 cross-references the shipped "never turn red green by deleting coverage" line rather than duplicating it (#4630).
- The `tm-pr-workflow` delivery chain now reads `accepted outcome -> optional issue -> worktree branch -> one cohesive PR -> applicable gates -> trusty-review gate -> squash-merge -> worktree cleanup`. The issue step is optional for docs/CI/chore work and a small explicitly-requested fix that completes in one PR, and required for features, reproduced defects, security work, cross-release dependencies, and work that must survive the current session (#4630).
- `rust-engineer` agent now scopes its Quality Bar to the crate under change
  (`cargo test -p <crate>`, not a bare unscoped `cargo test`), matching the
  crate-scoped-gate guidance the PM already ships in `core.md`. A crate-scoped
  run finishes well under the 10-minute tool timeout; a workspace run does not.
  - Adds a change-class table for widening scope deliberately, and an explicit
    "scope is for speed, never for hiding a failure" rule: narrowing the scope
    you *run* is fine, shrinking the coverage that *exists* — `#[ignore]`,
    `cfg`-gating, `--exclude`, dropping to `--lib` — is never allowed.
- The bundled `code-review-standards` skill now gives every review finding a three-way disposition, extending the shipped "fix it in the surfacing PR or drop it" rule with the third exit it left implicit. A finding ends as `Fix here` (corrected in the surfacing PR — the default for correctness, security, acceptance criteria, and regression coverage), `Parent` (kept with the work in flight as a PR comment or parent-issue checklist item, creating no durable artifact), or `Promote` (recommended for a standalone issue). `Promote` is a recommendation only: the reviewer never files, the Ticket-Promotion Gate in `tm-ticketing` decides, and the PM or user makes the prioritization call. An APPROVE verdict no longer implies tickets for its non-blocking MEDIUM/LOW observations (#4633).
- The critic verdict template carries the disposition explicitly, so the rule is checkable rather than decorative: the Findings table gains a Disposition column, every row must carry exactly one token, and a blank or missing cell is named an incomplete review — a reader of the posted verdict can tell per finding which of the three was chosen. A zero-finding APPROVE has no rows and so no dispositions to state (#4633).
- `tm-ticketing` gains the reciprocal pointer: a review or QA finding reaches the Ticket-Promotion Gate by exactly one route, the `Promote` disposition, and `Fix here` / `Parent` findings never reach it. The gate's five criteria stay stated once, in `tm-ticketing` (#4633).
- Risk is now the second input to phase entry in the PM instruction package. Three labels — Low (docs, comments, mechanical metadata), Normal (a localized behaviour change inside one package), and High (security, destructive or irreversible paths, persisted state, release/SemVer, or a contract another package depends on) — fold into the EXISTING skip conditions rather than forming a competing gate matrix. Where a skip condition is a size or simplicity heuristic, High risk means it does not hold: a 30-line change to a credential path is small and still earns its review. Phase 2's skip condition gains "not High risk" in both the canonical CORE phase table and its WORKFLOW restatement, so the two cannot drift. The labels carry no testing standard — the project's test ladder answers how much testing a change needs (#4633).

### Removed

- The bundled `crates/trusty-mpm/src/assets/instructions/INSTRUCTIONS.md`
  stub and the `FRAMEWORK_INSTRUCTIONS` constant/bundle-table entry that
  embedded it. This is the framework-owned launch-artifact stub, distinct
  from (and unrelated to) the project-level `.trusty-mpm/INSTRUCTIONS.md`
  override file retired by #4665. Its content never reached a live session:
  `tm install`/`tm launch` always overwrite the same on-disk path
  (`instructions/INSTRUCTIONS.md`) with the fully assembled system prompt in
  the same call, and the one legacy reader that treats it as optional
  (`build_instructions`) already discards its result for anything but a
  side-effect (#4286 split A).
- The five `.trusty-mpm/` PM instruction override files
  (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`,
  `MEMORY.md`, `INSTRUCTIONS.md`) are retired and no longer read. Project
  customization is named sections in the project's root `CLAUDE.md`.
  `.trusty-mpm/INSTRUCTIONS.md` also stops being a marker host; `CLAUDE.md` is
  the only one (#4286).
- The non-overridable framework floor, in full: `SectionId::is_floor()`, the
  `FloorNotFixed` and `OverridableAfterFloor` validation rules,
  `validate_floor_is_last`, `scripts/check_instruction_floor.sh`,
  `scripts/instruction_floor.sha256`, and
  `.github/workflows/instruction-floor-guard.yml` (plus its duplicated step in
  `ci.yml`). A project owns its own `CLAUDE.md`, so the floor was the appearance
  of a control rather than a control (#4286).

## [1.3.2] — 2026-08-03

### Added

- `tm doctor` now reports worktree DISK consumption, not just orphan counts. The new `worktree_disk` check totals the bytes held by every git-registered worktree under the managed workspace root and names how much of it sits on branches whose pull request already merged and holds no uncommitted or unpushed work. The existing `worktrees` check has never reported a byte, so it read identically whether the store held 4 GiB or the 1.1 TiB measured on 2026-07-21 (#2919).
- `tm session prune-worktrees --merged-prs` adds a merged-pull-request reclaim pass, the terminal-state trigger DOC-52 §3.4 specifies. It is off by default and independent of `--force` and `--discard-dirty`; nothing automatic ever runs it. A worktree is reclaimed only when all four gates pass: git's own `worktree list --porcelain` admits it (never the main checkout, a bare record, a locked worktree, or anything outside the managed project — ADR-0023), no session claims the path, its branch has a merged PR, and it holds no uncommitted, untracked, or unpushed work. Every gate fails closed — an indeterminate pull-request state (no `gh`, unauthenticated, or a truncated result page) refuses rather than reclaims — and each approved candidate is re-checked immediately before its own deletion (#2919).
- The `worktree_disk` probe bounds its own work rather than relying on an outer timeout. `tokio::time::timeout` cannot cancel a `spawn_blocking` task, so an outer bound returns a verdict on schedule while the byte walk keeps running and runtime shutdown waits for it — observed as a >20-minute hang against a real ~1 TiB worktree store. The walk now polls a deadline and stops itself, the budget fits inside the client's 10-second request timeout, and anything left unmeasured is disclosed as an explicit UNDERCOUNT pointing at `tm session prune-worktrees --merged-prs`, whose survey runs with no deadline (#2919).
- The pre-delete re-checks now re-read state FRESH per candidate instead of consulting the survey's captured slice. The survey byte-walks every registered worktree and does not finish in 600 seconds on this repository, so its liveness snapshot was minutes old by the time the first delete ran — a session attaching mid-sweep was invisible, and a `git worktree lock` applied mid-sweep was ignored (`remove_session_worktree`'s `remove_dir_all` fallback deletes locked worktrees regardless). Liveness, git's own registry, the ownership marker, the pull-request state, and the working tree are all re-read immediately before each individual deletion; an unreadable live session set refuses rather than reading as "nothing claims it" (#2919).
- Worktrees trusty-mpm did not provision are no longer advertised as reclaimable. The classifier applies the same ownership predicate the remover applies, so the harness-owned `.claude/worktrees/` store (out of scope per ADR-0020) is reported but excluded from `reclaimable_bytes` — previously `tm doctor` counted it and told the operator to run a command that then failed and left every one of them on disk (#2919).
- Pull requests opened from FORKS no longer authorize deleting local worktrees. `headRefName` is not an identity: a fork's `fix/foo` is a different branch from this repository's `fix/foo`. `gh` is now asked for `isCrossRepository` and cross-repository rows are dropped (#2919).
- Every `gh` invocation carries a 10-second timeout with its pipes drained on separate threads. `Command::output()` has no timeout, so a wedged network call would hang the uncancellable blocking task — the same failure shape already fixed for the byte walk (#2919).
- The `worktree_disk` probe reports `Unknown`, never `Ok`, when its walk was incomplete. Computing a healthy verdict from an undercounted figure inverted the check: the fuller the disk, the less of it fit the budget, and the more confidently it read healthy (#2919).
- The operator-invoked reclaim path resolves branches the bulk PR index could not reach with a targeted per-branch query, and `tm doctor` discloses its window. With 4526 pull requests against a 400-row index, every worktree older than the last 400 PRs previously read "state unknown" and could never be reclaimed (#2919).
- Fixed: the pull-request lookup passed `gh -C <dir>`, a flag `gh` does not have. Every call died at flag parsing with `unknown shorthand flag: 'C' in -C`, which yielded an unavailable index, which made every branch's state indeterminate, which blocked every candidate — so the merged-PR pass reclaimed zero bytes always, and `tm doctor` misreported an argv bug as a `gh auth status` failure. `gh` resolves its repository from the working directory; the call now sets `current_dir`. Verified end to end against a real 269-worktree store: 5 reclaimable worktrees, 17.8 GiB (#2919).
- The measurement budget is a duration allotted to the measurement phase rather than an instant fixed at survey start, and reclaimable worktrees are measured before blocked ones. Classification against a real store takes ~90 s, so a start-anchored instant had already expired before measurement began and nothing was measured at all; scan-ordered measurement then spent the budget on blocked worktrees and reported `reclaimable_bytes = 0` alongside 5 reclaimable worktrees (#2919).
- The pre-delete liveness read now runs after the pull-request lookups rather than before them, so the snapshot is single-digit milliseconds old when judged instead of being aged by a 322 ms-1.2 s network call (#2919).
- `reclaimable_bytes` is never reported without a measured-of-total qualifier. Ordering the measurement pass reclaimable-first moved the degradation rather than removing it: a single 17.8 GiB worktree consumed a whole 20-second budget, so four of five reclaimable worktrees still came back unmeasured and the byte sum was one worktree's size wearing the set's label — and because every other measurement was starved too it happened to equal `total_bytes`, reading as "all of it is reclaimable". The whole-survey UNDERCOUNT disclosure did not cover that. `tm doctor` and the CLI now print "N of M measured" beside the figure (#2919).
- `tm doctor` reports a worktree the probe never inspected separately from one whose pull-request lookup failed. Both read as an indeterminate state but need opposite actions — "raise the budget" versus "check `gh`" — and conflating them sent operators to `gh auth status` for a deadline problem (#2919).
- `tm doctor` gained an `asset_tier` check (25 checks, was 24) that fails when tm-owned agent files sit in a project's `.claude/agents/`. That tier outranks the canonical `$CLAUDE_CONFIG_DIR/agents/` deploy, so a stale or stub copy shadows the real agent while every presence-only check stays green — the [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) incident, where a 32-byte project stub beat the real 25 KB `rust-engineer`. Leftovers in `~/.claude/agents/` warn instead: a managed session relocates `CLAUDE_CONFIG_DIR` and never reads them. Agents are matched on their declared frontmatter `name:`, the key the harness itself resolves by, so a renamed file declaring a bundled name is caught and a bundled *filename* declaring a custom name is not; a file the ownership ledger records as the operator's is never reported, even on a name collision. Read-only — it never deletes, moves, or rewrites anything ([#4442](https://github.com/bobmatnyc/trusty-tools/issues/4442)).
- **`tm doctor --fix-skills`** redeploys drifted skills from the running binary's bundled assets and VERIFIES each repair by re-reading the file from disk — reporting success from a write's return value is the exact failure that produced this issue. Three constraints are enforced in code, not review: a FROZEN (hand-edited) skill is never silently overwritten and needs the explicit `--include-frozen` opt-in on top of `--fix-skills`; every overwrite is backed up first under `~/.trusty-mpm/backup-doctor-remediation-<timestamp>/`; and nothing is ever deleted. It has no worktree behaviour at all — `tm doctor`'s `worktrees` and `worktree_disk` checks stay report-only, because a fix mode that deletes what it thinks is orphaned is the 2026-07-21 `rm -rf .base/*` incident with a scheduler ([#4604](https://github.com/bobmatnyc/trusty-tools/issues/4604)).
- **`tm doctor` gains a `binary_provenance` check (28 checks, was 27)** answering "is what's running what you think it is?" — the observation doctor's liveness-plus-file-state health model never made. It reads cargo's own `$CARGO_HOME/.crates2.json` install ledger and compares it against the running executable. `Fail` when the same binary is provided by more than one install (two worktrees serving conflicting binaries, measured on `trusty-channels`), when the ledger's recorded version disagrees with what is running, or when a `cargo install --path` source directory has been reaped (no provenance and no upgrade path — one 2026-07-26 source lived under `/Users/masa/tmp/`, which macOS reaps). `Warn` for a live path or git install, which is invisible to registry update detection. `Unknown` — never `Ok` — when the ledger is unreadable or does not cover the binary, since a prebuilt-installer or package-manager install is genuinely unverifiable from here. Read-only; it never installs, moves, or deletes ([#4033](https://github.com/bobmatnyc/trusty-tools/issues/4033), ADR-0021).

- **`/health` now publishes the three-state `launchd_supervision` field** alongside the `supervised` bool it collapses. A bool cannot say "launchd could not be asked", and `CLAUDE.md`'s connection-safe restart convention tells operators to verify a restart by reading `/health` directly — so collapsing an unanswerable probe into `supervised: false` left the documented verification lying in exactly the case [#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469) exists to fix. The daemon also logs the reason at WARN. `tm doctor`'s orphan check deliberately does NOT consume the field: whenever the daemon reports a pid, that check decides from the client's own independent launchd query, where `None` and `Some(false)` are already equivalent — pinned by `authoritative_tier_treats_unknown_and_false_identically` so the omission reads as a decision rather than an oversight ([#4469](https://github.com/bobmatnyc/trusty-tools/issues/4469)).
- **`skill_staleness` resolves the deploy manifest's NESTED keys.** `skills::deployer::deploy_skills` records each multi-file skill's reference sibling under `<stem>/references/<file>.md`, not just a bare stem — 80 of the 132 keys in this machine's `~/.claude/skills` manifest. The first cut of this check compared only bare stems, so every nested key was `Unverifiable` and the check could never report `Ok` on a real install, no matter how clean it was. The reference map and the repair now key on the manifest key, with one shared `deployed_path` resolver so the audit and the repair cannot disagree about where a key lives ([#4604](https://github.com/bobmatnyc/trusty-tools/issues/4604)).
- **`binary_provenance` compares the running executable's PATH, not just its basename.** Attribution by name alone let a binary cargo never installed inherit the cargo record: on the [#4033](https://github.com/bobmatnyc/trusty-tools/issues/4033) host `which -a tm` returns `~/.cargo/bin/tm` (1.3.1) and `~/.local/bin/tm` (0.19.29), and had those versions coincided the check would have reported `Ok — installed from crates.io`. The running binary must now BE the file at `$CARGO_HOME/bin/<name>` (compared canonicalised, so a symlinked cargo bin dir is not a false mismatch); anything else reports `Unknown` and names both paths ([#4033](https://github.com/bobmatnyc/trusty-tools/issues/4033)).
- `tm install --reconcile-skills`: the explicit, opt-in adoption path. It prints every tier, skill, and FILE it will touch before touching anything, copies each file under `~/.trusty-mpm/backup-reconcile-skills-<timestamp>/` (the shape of the existing `backup-doctor-remediation-<date>/` convention, `moved-paths.log` included), adopts them into each tier's manifest, then re-runs the ordinary deploy so they pick up current bundled content. It never touches a skill whose stem matches nothing bundled. Nothing adopts automatically: content alone cannot distinguish an orphaned tm deployment from a deliberate customization, and silently overwriting the latter is the harm the tier system exists to prevent ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- `tm doctor` gains a `skill_unmanaged` check reporting UNKNOWN — never `Ok` — when any deploy tier holds a bundled-named skill its manifest does not track. This is a distinct state from `skill_staleness`, not a variant of it: staleness compares the bundled source against the deploy MANIFEST, so a skill missing from that manifest is outside everything it can see and it reported a clean `Ok` on the exact files that were unreachable. The check reports `Unknown` for an empty bundled roster too, rather than reading a state it cannot classify as healthy ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).

### Fixed

- The PM instruction floor's "Customizing PM Behavior" table advertised only the legacy `.trusty-mpm/*.md` override files and never mentioned the `CLAUDE.md` named-section channel that outranks them (`HOST_FILES` in `core/claude_md_sections.rs` scans `CLAUDE.md` before `.trusty-mpm/INSTRUCTIONS.md`), so the floor routed every PM session to the lower-precedence surface. `sections/non-overridable-rules.md` now documents named-section markers as the one customization channel, lists all eight recognized tokens and the three `fixed`-tier ones that can never be overridden, and states plainly that the `.trusty-mpm/` override files are still read by the current binary but [#4286](https://github.com/bobmatnyc/trusty-tools/issues/4286) removes them.
- Fixed a broken pointer in `sections/non-overridable-rules.md` to the deleted `PM_INSTRUCTIONS.md` (removed by [#4183](https://github.com/bobmatnyc/trusty-tools/issues/4183)) — repointed at the CORE section's Prohibitions table — and fixed mojibake (`SS` for `§`).
- `core.md`'s Model Selection Protocol undercounted the model set ("sonnet or haiku", omitting opus — the mandatory choice for coding tasks per its own routing table two lines below), and its PM Allowlist listed `push` as an allowed git op, contradicting P7 ("branch/push/rebase/merge/tag -> Version Control") one section up. Both corrected to match the rest of the floor.
- `tm doctor`'s `search` check no longer hardcodes a literal expected index id
  (`"trusty-mpm"`, the crate name) — it now derives the expected id from the
  project itself, the same rule `core::session_launch` and trusty-search's own
  `detect_project` use, so a repo registered under a different index id (e.g.
  this repo's `trusty-tools`) no longer permanently warns "index missing"
  against a healthy, fully-indexed project
  ([#4003](https://github.com/bobmatnyc/trusty-tools/issues/4003)).
- Managed hook commands and `statusLine.command` can no longer be persisted pointing at a binary in a system temp directory. A `cargo test --no-run` artifact in an agent harness's scratchpad (`/private/tmp/claude-<uid>/…/scratchpad/…`) passed the ephemeral-path guard, so `settings.json` was written with hook and statusline commands that ran a dead libtest harness on every hook event (#4485; #4492 is the same root cause reaching `statusLine.command`).
- `TmConnector::create_session` no longer aborts after 10s on a call that provisions a workspace. The connector built its client from `default_client()` and never overrode the per-request bound, so `POST /api/v1/sessions/managed` — where the daemon runs `WorkspaceProvisioner::provision` synchronously (git clone, worktree add, agent deploy, harness spawn) — inherited `DEFAULT_REQUEST_TIMEOUT`, a bound sized for interactive point reads. The identical route in `DaemonClient::spawn_managed_session` has opted into the 180s `PROVISION_REQUEST_TIMEOUT` since #2471; the connector was never updated to match. Success was therefore contingent on the machine being idle — the hermetic `create_session_full_lifecycle` test measured 9.23–9.54s against the 10s cliff — and past it the client aborted mid-provision, surfacing as `ConnectorError::Transport` while the daemon kept working and orphaned the session it had already started. `create_session` now carries the same override, sharing the constant so the two call sites cannot drift (#4488).
- `TmConnector::with_client` reuses a caller-supplied `reqwest::Client` instead of silently replacing it, mirroring `DaemonClient::with_client`. It is also the seam the new `create_session_outlives_the_default_request_timeout` regression test uses to pin the bound above in milliseconds rather than minutes, with a `list_sessions_is_bound_by_the_client_level_timeout` negative control proving the override stays scoped to the provisioning route rather than un-bounding the connector and reintroducing #2471 (#4488).
- The Prohibitions (`P1`-`P11`) and Circuit Breakers tables — the PM's entire delegation-enforcement authority — shipped inside the `project`-tier `core` section, so a three-line `CLAUDE.md` block (`<!-- TRUSTY-MPM: CORE START v=1 -->` … `END`) deleted both from the delivered prompt and validated cleanly, while the framework floor went on asserting that "all prohibitions defined in the CORE section's Prohibitions table are BINDING" — a pointer to content no longer in the prompt. The `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md` full-replacement path deleted them the same way. Both tables now live in their own `fixed`-tier `enforcement` section inside the non-overridable floor (`sections/enforcement.md`, byte-pinned in `scripts/instruction_floor.sha256`), so no override at any tier can remove them and `base_pm()` carries them into every legacy composition branch. The rest of `core` — response format, prose style, allowlist, project context — stays project-customizable ([#4573](https://github.com/bobmatnyc/trusty-tools/issues/4573)).
- `tm sessions instructions` — the command an operator runs to ask "why didn't my override apply?" — registered no tracing subscriber, so `ProjectOverrides::log`, `warn_unapplied` and `read_override` emitted their applied/declined/shadowed diagnostics into a void: `RUST_LOG=trusty_mpm=info tm sessions instructions --dir <project>` produced zero bytes on stderr. In the [#4573](https://github.com/bobmatnyc/trusty-tools/issues/4573) reproduction the operator's only signal that the authority tables had been stripped was their absence from 28 KB of output. The command now registers a stderr-only subscriber (default filter `warn`, so declines are visible without setting `RUST_LOG` and a clean project stays silent); stdout still carries only the resolved prompt, which the daemon and MCP JSON-RPC framing require.
- The non-overridable PM instruction floor is now pinned byte-exactly in CI. `scripts/check_instruction_floor.sh` verified it with three `grep -qF` substring checks, which passed a floor whose every rule had been inverted ("always emit `Co-Authored-By: Claude`"), a floor reduced to the three bare needles, and a floor wrapped entirely in an HTML comment — all three mutations were run against the old guard and all three passed. The guard now diffs a sha256 per floor artifact against the committed `scripts/instruction_floor.sha256`: every fixed-tier `sections/*.md`, a canonical projection of the manifest's fixed sections and their blocks (so retiering a floor section to `project` or repointing it fails without any section file changing), and the guard's own workflow. A deliberate floor change regenerates the pin with `bash scripts/check_instruction_floor.sh --update` and commits it alongside the edit. The guard also runs inside ci.yml's required `Format check` job, so deleting its own workflow no longer disarms it (#4581).
- The shipped `tm-workflow` skill described the pre-#4183 four-flat-file instruction bundle (`PM_INSTRUCTIONS.md`, `WORKFLOW.md`, `AGENT_DELEGATION.md`, `BASE_PM.md`), which no longer exists — the source of truth is now `pm-instruction-package.json` (schema v2) plus per-section prose files, composed by `bundled_pm_package.rs`. Rewrote "How the PM Prompt Is Assembled" and swept every other stale reference to describe the current model: the JSON manifest, the `sections/*.md` prose store, the per-launch composed prompt stashed at `.trusty-mpm/last-instructions.md`, and the two live project-override channels — `CLAUDE.md` named-section markers and the still-functional legacy `.trusty-mpm/` per-file overrides ([#4582](https://github.com/bobmatnyc/trusty-tools/issues/4582)).
- The agent roster is resolved by exactly one function, so the count `tm session
  start` prints can no longer disagree with the roster the PM receives. The
  printed number came from a single-directory scan of the tm-managed deploy tier
  while the delegation section delivered to the PM was a three-tier union
  (project `.claude/agents` + `$CLAUDE_CONFIG_DIR/agents` + the operator's
  `~/.claude/agents`) — two independent implementations of the same question,
  measured live at 37 printed against 42 delivered. `build_instructions` now
  takes the project rather than an agents directory and resolves the roster
  through `delegation_authority::resolve_roster`, which the PM prompt and
  `tm doctor` also call
  ([#4588](https://github.com/bobmatnyc/trusty-tools/issues/4588)).
- Agents are no longer excluded from the delegation roster because their
  frontmatter `role:` begins with `base`. Foundation templates are identified by
  a case-insensitive `base-` file-stem prefix — the hyphen is load-bearing, so
  `baseline-analyzer`, `base64-decoder` and `basecamp-sync` are ordinary
  delegatable agents rather than templates, and every excluded file is logged at
  debug level with its directory instead of vanishing silently. The frontmatter
  rule silently deleted
  three real, deployed, dispatchable agents — `memory-manager`,
  `mpm-agent-manager`, `mpm-skills-manager` — from every roster the PM ever saw
  (34 advertised where 37 were deployed), and it could not be fixed by editing
  assets: the roster unions two tiers whose frontmatter tm does not author, so
  the same rule would eat an operator's own agent with no recourse
  ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
- The bundled `agent-delegation` instruction section no longer tells the PM that
  `memory-manager`, `mpm-agent-manager` and `mpm-skills-manager` are hidden by a
  `role: base` filter. That paragraph shipped into every PM prompt and, once the
  filter was removed, contradicted the live roster rendered directly beneath it
  ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
- `tm doctor`'s `agents` check reports the delegatable roster size alongside the
  deployed file count on every status — including the empty-deploy-tier failure,
  where the roster can still be non-empty because the project and generic
  `~/.claude/agents` tiers are independent of the deploy tier — resolved by that
  same function, and reports it as `unknown` rather than guessing when no project
  directory scopes the probe. A
  file count is a deploy fact, not a routing fact; reporting only the former is
  how a 42-file, 34-agent install read as healthy
  ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
- `crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md` opened with a header pointing customization at `.trusty-mpm/AGENT_DELEGATION.md` / `~/.trusty-mpm/AGENT_DELEGATION.md` and self-referencing a `System default` path (`assets/instructions/AGENT_DELEGATION.md`) that no longer exists — the content moved to `sections/agent-delegation.md` when the instruction package was split into named sections. Project customization is named sections in the root `CLAUDE.md`; the `.trusty-mpm/` files are a residual read path tracked for removal by [#4286](https://github.com/bobmatnyc/trusty-tools/issues/4286), not the way to customize this section. The header now states the file's actual two-part shape: a STATIC hand-authored routing table for universal system agents, and an ENRICHED roster trusty-mpm appends at composition time by scanning deployed agents on disk (project `.claude/agents`, managed `CLAUDE_CONFIG_DIR/agents`, framework agents dir), which picks up manifest-declared and user-installed agents alike and is required for composition to succeed — noting the one known gap where `role: base` frontmatter hides three real agents ([#4589](https://github.com/bobmatnyc/trusty-tools/issues/4589)).
- **`tm doctor`'s `skill_staleness` check compared a stale cache against itself and reported a drifted skill as clean** ([#4604](https://github.com/bobmatnyc/trusty-tools/issues/4604)). It called `stale_skills(paths.skill_source_dir(), …)`, which for every binary-only install resolves to `~/.trusty-mpm/framework/skills/` — an EXTRACTION CACHE — and compared it against the checksum recorded in the deploy manifest. Both sides can be the same stale content. Measured 2026-08-01: PR [#4583](https://github.com/bobmatnyc/trusty-tools/issues/4583) rewrote the bundled `tm-workflow` skill and shipped in 1.3.1, but the cache had not refreshed since before the merge, so its sha256 was byte-identical to the manifest's; the check correctly reported "no drift" against the wrong reference point and named only two other skills, which reads as an all-clear for the one it missed. The reference is now the RUNNING BINARY's own embedded asset (`bundle::ALL`) — a compiled-in table cannot lag the binary containing it — with a checked-out, git-tracked `agents/skills` submodule the one legitimate override. The comparison now reads the DEPLOYED FILE rather than the manifest checksum, so a hand-edit is visible, and it covers every deploy tier (`$CLAUDE_CONFIG_DIR/skills`, `~/.claude/skills`, the project's `.claude/skills`) rather than one.
- **Drift that a redeploy repairs is now reported separately from drift that is FROZEN.** A skill hand-edited after deployment no longer matches the manifest checksum, so `deploy_skills`' "checksum differs → user-modified → skip" rule means `tm install` will never touch it — it can stay stale forever with no signal. The two states carry different messages and different remediation ([#4604](https://github.com/bobmatnyc/trusty-tools/issues/4604)).
- **Anything the check cannot verify now reports `Unknown`, never `Ok`.** A managed skill with no embedded counterpart (a user-tier or renamed skill this binary does not ship), an unreadable deployed file, or an empty reference set are all indeterminate — and `Unknown` ranks above `Warn`, so one of them can never leave the aggregate report reading healthy ([#4604](https://github.com/bobmatnyc/trusty-tools/issues/4604)).
- A bundled skill deployed to a location where it is absent from that location's `.trusty-mpm-skills-manifest.json` was classified project-custom by the skill-tier planner and excluded from `plan.bundled_deploy` entirely, before `deploy_skills_filtered` ran — so the deploy loop never executed for that stem, the ownership check never got to distinguish "the operator's" from "orphaned bundled", and no `tm` command could reach the file. Measured 2026-08-01: `tm install`'s deploy step printed only `= tm-slack-canvas-delivery (unchanged)` while `$CLAUDE_CONFIG_DIR/skills/tm-workflow/SKILL.md` (mtime 07-03) and `~/.claude/skills/tm-workflow/SKILL.md` (mtime 07-17) both still carried text [#4583](https://github.com/bobmatnyc/trusty-tools/issues/4583) had removed. `$CLAUDE_CONFIG_DIR/skills` is the roster every managed session reads, so a fix that landed on main, shipped in a signed binary, and passed 24 CI checks reached zero sessions ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- `tm install` now REPORTS those skills instead of omitting them. Each prints `! <stem> (untracked at <tier> — not managed, not refreshed; see \`tm install --reconcile-skills\`)`. Previously such a skill appeared as neither deployed, skipped, NOR unchanged: it was excluded upstream by the tier planner, not skipped by the ownership check, so nothing in the output referred to it at all ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- Coverage spans every skill deploy target, not just the one the command happens to write. The new `core::skill_deploy_tiers` enumerates `$CLAUDE_CONFIG_DIR/skills`, `~/.claude/skills`, and the project's `.claude/skills`, deduplicated, and the reporter, the reconcile, and `tm doctor` all consume that one list ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).

### Changed

- Deduplicated the commit/PR attribution footer text, which appeared three times in the resolved PM prompt (`core.md`, `workflow.md`, `framework-guaranteed-conventions.md`). The two non-canonical copies now point to the single canonical statement in `framework-guaranteed-conventions.md`; the footer text itself is unchanged.
- `tm ls` no longer re-reads the machine-global deployed-agent directory once per session. Bundled agents deploy into a single `CLAUDE_CONFIG_DIR` tier that #4409 deliberately exempted from the per-workspace path rewrite, so every session in a listing resolved the same 42 files and read them independently — on a 32-session fleet that was 1344 opens and 30.2 MiB of the 41.9 MiB the listing moved, producing 32 byte-identical answers. The listing now reads that tree once and shares it across the fan-out, exactly as #2444's review already did for the catalog side. Measured on a 32-session synthetic fleet, timing both paths alternately in one process: deployed-side file reads drop 3008 → 1706 (-43%), bytes read drop 41.9 MiB → 12.7 MiB (-70%), and the warm fan-out median goes from 230-236 ms to 147-152 ms across three independent runs (1.53-1.57x). Cold `tm ls` latency scales with the file count this removes (#4322).
- No other call shape pays for the listing's speed-up. Pre-hashing the deployed agent tree is a win only when the result is shared across sessions, so it applies to the fleet listing alone; every single-target caller (`/health`, `tm catalog apply`, and the single-session fetch `tm session resume` reads) hashes on demand and therefore still reads exactly the agents its manifest selects — byte-for-byte the same reads as before. A manifest that selects a subset of the catalog would otherwise have read every catalog agent instead of just its own (#4322).
- The staleness comparison is unchanged in what it reports: the shared read supplies input bytes only, and each session still applies its own manifest selection and ignore predicates afterwards, so per-session verdicts stay independent. Sharing is scoped to a single listing and nothing is retained between calls, so a change to a deployed asset is visible on the very next `tm ls` — there is no cache-invalidation window (#4322).
- the activity classifier now issues its LLM call through the unified `trusty_common::inference::InferenceAdapter` instead of the legacy `chat::ChatProvider` SSE pump ([#4427](https://github.com/bobmatnyc/trusty-tools/issues/4427))
  - One blocking `InferenceAdapter::chat` call replaces the `chat_stream` +
    mpsc pump. The classifier only ever consumed the finished string, so
    streaming bought nothing and cost a truncated-mid-stream failure mode
    (#3757) that had to be detected and re-reported by hand.
  - Credentials now resolve through the shared ladder (env > `.env.local` >
    secure store) rather than a direct `OPENROUTER_API_KEY` env read. A daemon
    whose key lives only in `.env.local` or the secure store — and which
    previously reported "OPENROUTER_API_KEY is not configured" — now classifies
    normally. The `MissingApiKey` error variant and its message are unchanged,
    so every caller's degrade branch behaves as before.
  - Per-check cost metrics carry real token counts. The SSE path could not see
    usage and hard-coded `(0, 0)`, which made every activity cost tally read
    zero.
  - `OpenRouterClassifier` moved from `activity::monitor` to a new
    `activity::classifier` module (both files were headed past the 500-SLOC
    cap); it is re-exported from `crate::activity`, which is now the stable
    import path.
- Delegation is now stated as a DEFAULT WITH A BUDGET rather than an absolute prohibition, resolving four contradictions that shipped together in the same PM prompt. The floor asserted "never direct impl" and "no cost-saving, 'trivial change', or 'documented command' exceptions" while `core` granted a user override and `enforcement` granted a 3-file `pm_guard` budget — all four in one prompt. The owner's ruling replaces the absolute phrasing: the user can always override, and the PM delegates when a task will take more than 3 direct ACTIONS, or when it is unable to complete the task in 3. That second clause is a mid-flight handoff rule, stated explicitly — a task begun on a 3-action estimate that stops holding is handed off at that point, never carried to a fourth direct action. The budget is denominated in actions, not files; `pm_guard`'s per-turn file-change limit is described as the mechanical floor of a broader rule rather than as the rule itself, because the hook sees files and the budget counts actions. `P2`-`P4` and `P6`-`P11` are routing rules to specific agents, not budgeted direct actions, and stay absolute (#4594).
- Workflow phases no longer contradict themselves on whether they are mandatory. `workflow.md` declared Code Analysis and QA MANDATORY while `core.md`'s phase table gave each a skip condition. Every phase is now CONDITIONAL, the CORE phase table is named canonical for whether a phase runs, and `workflow.md` covers how it executes. The QA Verification Gate stays blocking when phase 4 runs; skipping it moves where the evidence comes from (the engineer's raw test output) and never removes the evidence requirement (#4594).
- Every agent named in a routing rule is now the deployed `subagent_type`, verified against the bundled agent files on disk rather than another document. `API QA`, `Documentation Agent`, `Code Analysis`, `Local Ops`, `Version Control` and the rest were prose titles that fail to dispatch. `ticketing` was required by `P6` but missing from the routing roster, and the keyword table sent "ticket"/"issue" to Version Control in direct contradiction of `P6` — that row now splits bookkeeping (`ticketing`, P6) from PR mechanics (`version-control`, P7). `code-analyzer` and `code-critic` are named as separate, non-interchangeable agents (#4594).
- The PM Allowlist no longer contradicts the action budget. Its only write row read "NOT source code, NOT bulk edits" and sat near the TOP of the compiled prompt — read first — while the floor ~1000 lines later budgeted P1/P5 at 3 direct actions "including one Edit, one Write". A prompt asserting both let the PM cite whichever suited it. The table is now framed as what is UNBUDGETED, with an explicit budgeted source-edit row carrying both halves of the rule (#4594).
- The bundled `tm-circuit-breaker` skill no longer restates the retired absolute rule. It said `.git/COMMIT_EDITMSG` was "the only Edit target the PM may touch directly" and BLOCKed any Edit/Write over ~5 lines — and the PM receives that skill alongside the instruction package, so it would have re-introduced the contradiction from a second channel. CB#1 is now "Implementation Past the Budget", triggered by a 4th direct action including the mid-flight case; CB#14 no longer implies an absolute Edit/Write ban. `tm-tool-usage-guide` carried the same stale framing and is corrected. `tm-circuit-breaker` also pointed at `PM_INSTRUCTIONS.md` for the canonical Circuit Breakers table, which #4183 deleted and #4623 moved into the fixed-tier enforcement floor section; repointed (#4594).
- Version-pinned model identifiers are gone from the shipped prompt. `claude-sonnet-4-5` / `claude-opus-4-5` / `claude-haiku-4-5` and the "5x cheaper … 75x cheaper" ratios were embedded as prose and had gone stale. The prompt now instructs the PM to pass the tier ALIAS and points at `expand_model_alias` and `[models.tiers]` in `~/.trusty-mpm/config.toml` as the source of truth, so a tier move cannot leave a stale id behind (#4594).

## [1.3.1] — 2026-07-31

### Added

- `tm doctor` gained a `transcript_saving` check (24 checks, was 23) that fails
  when any `tm` launch line would leave transcript saving disabled — the defect
  it guards was silent until Claude Code began warning about it. It reads the
  scrub set out of the real launch commands rather than restating a constant,
  names the offending builder on failure, and fails in both directions:
  under-scrub costs the session its transcripts, over-scrub costs it the agent
  roster ([#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451)) or the
  OAuth token ([#2246](https://github.com/bobmatnyc/trusty-tools/issues/2246)).
- `tm doctor` gains an `agent_reachability` check that fails when the bundled roster is unreachable ([#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451))
  - The existing `agents` and `deployment` checks are presence-only — they
    counted 42 files and diffed them against the canonical roster, and both
    reported green throughout the outage above. The new check asserts the one
    thing they cannot: that the settings tier the roster deploys into is a tier
    the managed spawn's `--setting-sources` flag actually loads. It reads both
    sides from production code, so moving the deploy destination without
    updating the flag (what happened here) is a hard `Fail`, not a silent
    regression.
- bundled `tm-slack-canvas-delivery` skill — codifies the Slack canvas delivery protocol (resolve destination, create with native `channel_id` binding to survive free-tier restrictions, post the link, verify the send) so canvas creation alone is never mistaken for delivery ([#4447](https://github.com/bobmatnyc/trusty-tools/issues/4447))
- `tm` CLI accepts unambiguous abbreviated subcommands, e.g. `tm doc` for `tm doctor` ([#4398](https://github.com/bobmatnyc/trusty-tools/issues/4398))
  - Turns on clap's `infer_subcommands` for the top-level `Cli`, propagating to every nested action enum. Exact matches still win over prefix inference, so ambiguous-prefix pairs (`hook`/`hooks`, `project`/`projects`, `session`/`sessions`, `status`/`statusline`) keep resolving to their own exact command.

### Fixed

- `tm start` / `tm restart` no longer spawn an unsupervised daemon that can seize the port from launchd (closes [#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230))
  - `commands::daemon::start` was the one client-side daemon-spawn path with no
    launchd awareness at all — the MCP stdio bridge has refused to auto-spawn
    since [#2486](https://github.com/bobmatnyc/trusty-tools/issues/2486) and
    `guided_autostart` has nudged launchd since
    [#1900](https://github.com/bobmatnyc/trusty-tools/issues/1900), while
    `start` spawned a detached bare `tm daemon` unconditionally. That is how the
    #4230 orphan was created: PID 98606, `PPID 1`, `cwd=$HOME`, holding
    `127.0.0.1:7880` for two days and answering `/health` 200 with a stale 1.0.2
    image while launchd's `com.trusty.mpm` reported `state = not running`. A
    fresh signed install and its behavioural verification both passed against
    the binary the install had just replaced.
  - Both `start` and `restart` now refuse when a DAEMON launchd unit is
    registered, naming the unit that actually exists on that host, its
    `launchctl kickstart` recipe, and the `tm daemon --force` opt-in. The check
    runs BEFORE `restart`'s `pkill`, so a refusal never tears the supervised
    daemon down and then declines to bring it back. The decision is keyed on
    plist presence alone — no supervision heuristic — because the callee-side
    [#4397](https://github.com/bobmatnyc/trusty-tools/issues/4397) guard folds in
    `is_launchd_supervised`, whose `XPC_SERVICE_NAME` and `getppid() == 1` prongs
    can BOTH report `true` for a non-launchd child.
  - `com.trusty.mpm.supervisor.plist` no longer counts as "launchd owns the
    daemon". That plist runs `tm supervisor`, a passive fleet observer that never
    starts a daemon, so counting it made every consumer wrong on a `tctl install`
    host whose only plist is the supervisor's: the refusal fired with a false
    claim and prescribed a `com.trusty.mpm` unit that does not exist there. This
    also removes a latent false refusal in the #4397 child guard and a latent
    false `no_spawn` in the #2486 bridge guard on those hosts. A drifted daemon
    label still resolves, matching `install-trusty-mpm-signed.sh`'s existing
    `resolve_mpm_plist` rules.
  - `tm stop` is not refused — stopping the daemon is a legitimate request — but
    now warns that launchd will NOT respawn it
    (`KeepAlive.SuccessfulExit=false`) and names the command that will. That
    SIGTERM is how the incident's `com.trusty.mpm` came to report `not running`
    for four days before anything else went wrong.
  - Every remediation that prescribed `tm restart` now resolves the verb for the
    calling host — `tm doctor`'s staleness line (which printed "run `tm restart`"
    two lines above the new orphan check), the three `tm manager` upgrade hints,
    the session-picker stale-numbering warning, and the bundled
    `tm-cli-operations` skill that agents read.
  - The #2486 bridge guard did NOT regress. The orphan's own
    `{"supervised":false}` proves a plist was registered when it started
    (`supervised` is `false` only in that case), which means the bridge's
    `no_spawn` was necessarily set and it could not have been the spawner.
- new `tm doctor` check `daemon_orphan` catches a daemon that answers `/health` but is not the one launchd runs ([#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230))
  - It compares WHO ANSWERED (`/health`'s new `pid`) against WHO LAUNCHD RUNS
    (`launchctl`), two independent sources, so neither the daemon nor launchd
    alone can make the check pass. The daemon's own `supervised` flag is a
    FALLBACK only, used when the responding daemon is too old to report `pid`:
    its `getppid() == 1` prong reads `true` for a genuine orphan, so a `true`
    there yields `Unknown`, never a pass.
  - The launchd probe answers in three states, not two. "launchd has this unit
    and its job is DOWN" is the #4230 incident and yields a hard `Fail` with a
    `kill -TERM` remediation; "launchd could not be asked" — `launchctl` missing
    from `PATH`, sandboxed, or the unit invisible in the caller's bootstrap
    domain — yields `Unknown` and prescribes nothing. Collapsing the two would
    make `tm doctor` order the operator to kill a correctly supervised daemon,
    which `KeepAlive {SuccessfulExit: false}` would then decline to restart. A
    `launchctl list` that exits 113 is retried against an explicit `gui/<uid>`
    domain before the probe gives up.
  - The daemon launchd label is resolved per host rather than hardcoded, and a
    drifted label must have `ProgramArguments` that actually run `daemon` — so
    neither the supervisor plist nor a `logrotate` sibling can be mistaken for
    the daemon unit, and every remediation names a unit that exists on this host.
- four non-hermetic test fixtures no longer redden the shared gate for reasons unrelated to the change under test (closes [#4306](https://github.com/bobmatnyc/trusty-tools/issues/4306), [#4415](https://github.com/bobmatnyc/trusty-tools/issues/4415); refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451))
  - The dead-daemon address fixture no longer binds an ephemeral port and drops
    it — a released port returns to the OS pool, so another binder could be
    handed it and the "nothing is listening here" premise silently expired
    (measured: reassigned and accepting connections after one wrap of the
    ephemeral range). All three duplicate copies, across 13 call sites, now
    share one fixture on `127.0.0.1:1` — privileged, so unbindable, and outside
    every ephemeral range — matching the convention already used by
    trusty-review and trusty-analyze.
  - `telegram::supervisor`'s healthy-run backoff-reset test runs on tokio's
    paused clock instead of asserting a 20ms wall-clock bound against a 2ms
    policy base, which measured scheduler jitter rather than backoff state.
    The assertions are now exact equalities against literal delays (2ms, 8ms,
    32ms), which also catch a partial reset the old bound accepted.
  - The plain-mode banner tests, and `pm_guard_bash`'s TMPDIR/HOME expansion
    test, joined the default `#[serial]` group. The banner tests' file-local
    mutex serialised them against each other and nothing else, so they raced the
    other `$HOME` mutators in the same test binary — 19 HOME-mutating statements
    in 8 fns across 4 files in the `tm` bin target. One banner test also seeded
    `banner.txt` into the ambient home instead of its own sandbox, and the
    `pm_guard_bash` test left `HOME` repointed for the rest of the binary; it now
    restores both `HOME` and `TMPDIR` through an RAII guard, so an assertion
    panic cannot skip the cleanup.
  - Env-isolation audits are scoped per TEST TARGET, not per crate: `#[serial]`
    coordinates threads inside one test binary and process-global `$HOME` is per
    process, so neither can span targets. The trusty-mpm lib target's own HOME
    mutators were never in this race.
- sessions launched via the `trusty-mpm` binary are now auto-discovered, matching `tm`-launched sessions (closes [#4058](https://github.com/bobmatnyc/trusty-tools/issues/4058))
  - `daemon::discovery`'s tmux-pane predicate only recognised `tm`, so an
    identical session launched under the crate's other `[[bin]]` target
    (`trusty-mpm`) never appeared in `GET /sessions` or other discovery
    lists — no error, just silent invisibility.
  - The crate had four independent hand-copies of "this crate's own binary
    names" (`discovery`'s list, hooks' `MPM_BIN_NAMES`, the statusline
    resolver's `STATUSLINE_BIN_NAMES`, and an inline check in the `tm stop`
    daemon-PID scan) and they had already drifted once. Three now read from
    one canonical `core::own_binary_names::OWN_BINARY_NAMES` constant; hooks'
    list keeps its own array (its PATH-lookup order differs) pinned to the
    same set by a test.
- `session_status` MCP tool no longer reports `delegation_count: 0` for a managed session with subagents genuinely in flight (closes [#4141](https://github.com/bobmatnyc/trusty-tools/issues/4141))
  - The handler resolved a `ManagedSessionId` and passed it straight into
    `delegations_for`, but hook-observed delegations are keyed by the Claude
    session UUID — a different identifier space bridged only via
    `SessionRecord::claude_session_id` /
    `session_start_correlation::correlate_session_start`. The mismatch made
    the surface that answers "what is in flight right now" structurally
    blind to its own feature.
  - The managed branch now reads BOTH key spaces — the managed id
    `agent_delegate` (#1976) writes under, and the bridged Claude UUID
    hook-observed delegations use — and unions them, deduplicated. When the
    bridge itself is missing (no correlated Claude session id yet),
    `delegation_count` and `delegations` both report an explicit `null` with
    `delegation_lookup: "unbridged"` rather than a confident-looking
    `0`/`[]` — an unbridgeable lookup must never be indistinguishable from
    "genuinely none in flight".
- managed sessions no longer silently lose their Claude Code transcripts (closes [#4467](https://github.com/bobmatnyc/trusty-tools/issues/4467))
  - Every managed spawn now scrubs the process-local session markers Claude Code
    injects into its child processes. When `tm` was invoked from inside a Claude
    Code session it inherited `CLAUDE_CODE_CHILD_SESSION` and passed it through,
    which made the spawned `claude` turn session persistence OFF: no native
    `--resume`, no `--continue`, no `/rewind`, and the session never appeared in
    `--resume` listings. Since tm's own pause/resume writes only a condensed
    summary, a session that died lost everything since its last snapshot.
  - Scrubbed: `CLAUDE_CODE_CHILD_SESSION` (the sole transcript-saving trigger),
    `CLAUDE_CODE_SESSION_ID`, `CLAUDECODE`, `CLAUDE_PID`, `CLAUDE_EFFORT`,
    `CLAUDE_CODE_EXECPATH`. Deliberately NOT scrubbed: `CLAUDE_CONFIG_DIR` (tm
    relocates it on purpose — [#4455](https://github.com/bobmatnyc/trusty-tools/issues/4455)
    depends on it to keep the bundled roster in a settings tier managed sessions
    load), `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS` (an
    operator-facing knob) and `CLAUDE_CODE_ENTRYPOINT`.
  - Applied to every launch line, not just the default one: `spawn_command` and
    `resume_command` (a spawn-only fix would leave every resumed session broken),
    the in-place bare-`tm` relaunch, the headless stream-JSON backend, the
    `tm launch` / `tm connect` launch line, the pane-relaunch line, `tm run`,
    `tm session start`'s in-place pane, and the `DaemonClient` launch/connect
    line behind the TUI and bot surfaces.
  - `tm run` mattered most: it spawns `claude` with no tmux, so Claude Code's
    `tmux show-environment -g` escape hatch returns false immediately and the
    suppression fired there every time rather than depending on the tmux
    server's environment.
  - Every launch line is now built in one place. Three call sites had each
    hand-written their own `claude …` string, and all three silently saved no
    transcript — a hand-built line is invisible to the `transcript_saving`
    check, which can only read builders. A new test walks the crate's source
    and fails when a `claude` launch site appears that is not accounted for,
    so the next one is a build failure rather than a silent gap.
- delegation-tracker `agentId` presence-check no longer accepts `null`/`""` (closes [#4163](https://github.com/bobmatnyc/trusty-tools/issues/4163))
  - `classify_dispatch` decided "we have an agent id" with key-presence alone
    (`r.get("agentId").is_some()`), while `on_launched`'s consumer rejected a
    non-string value and `on_subagent_stop`'s consumer rejected an empty
    string. A `{"agentId": null}` or `{"agentId": ""}` dispatch response took
    the `Launched` branch and pinned the delegation `Running` with a handle no
    `SubagentStop` can ever quote back, burning the full 6h staleness window as
    a phantom in-flight entry. Both call sites now share one extraction,
    `usable_agent_id` (non-null, non-empty string), so the presence-check and
    the consumers are structurally incapable of disagreeing again.

- managed sessions can reach the bundled agent roster again — every delegation was degrading to `general-purpose` (closes [#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451))
  - Daemon-managed spawns now launch with `--setting-sources user,project,local`
    instead of `project,local`. Claude Code discovers subagents per settings
    tier, and `$CLAUDE_CONFIG_DIR/agents` — where [#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)
    moved the bundled roster — IS the `user` tier, the one tier the old flag
    dropped. A session showed only the five Claude Code built-ins and answered
    `Agent type 'rust-engineer' not found` while all 42 agent files sat
    correctly on disk.
  - Re-admitting the `user` tier does not re-admit the operator's global
    `~/.claude/settings.json` hooks that [#1269](https://github.com/bobmatnyc/trusty-tools/issues/1269)
    excluded: managed spawns relocate `CLAUDE_CONFIG_DIR`, so the `user` tier
    resolves to the tm-owned config home. Launch paths that do NOT inject
    `CLAUDE_CONFIG_DIR` (`tm launch`, `tm connect`) keep `project,local`
    unchanged — the flag is now selected from that one fact rather than
    hard-coded per call site.
- Abbreviated-subcommand inference (#4398) made the hidden `internal-spawn-disclaimed` shim reachable via any unambiguous prefix (`tm int`, `tm inte`, `tm internal`), which would `posix_spawn` an arbitrary program with macOS TCC responsibility disclaimed. `main` now rejects any invocation whose raw first argv token isn't the exact, full subcommand name before the shim ever runs, while the daemon's own literal self-invocation keeps working unchanged.
- JSON instruction manifest (schema v2) with owner language rules ([#4386](https://github.com/bobmatnyc/trusty-tools/pull/4386)) ([`838d001`](https://github.com/bobmatnyc/trusty-tools/commit/838d00150e4ac1d2f31e5748bf8a85c05b4e78e7))
  - Adds three owner language rules as inline `text` blocks: **Clickable
    References** (`core` — issue/PR/ticket/commit references always render as
    clickable links, never bare numbers), **Banned Word "honest"** (`core` —
    banned from PM responses/delegation briefs/review instructions), and
    **Opportunistic Fixes** (`workflow` — an easy fix found while working on a
    file is made in the same work, never spun into a new issue).
  - Schema v2 adds a `{"kind":"file","path":"…"}` body variant alongside
    `text`/`generated`, resolved through a compile-time `include_str!` table
    (`instruction_pipeline::SECTION_SOURCES`) so a renamed section is a
    compile error.
- reconcile worktrees on a git-derived key, report-only ([#4288](https://github.com/bobmatnyc/trusty-tools/pull/4288)) ([#4382](https://github.com/bobmatnyc/trusty-tools/pull/4382)) ([`7babc82`](https://github.com/bobmatnyc/trusty-tools/commit/7babc82f7353d67327e08626844b91c4eba5187b))
  - Classifies every git-registered worktree `LIVE`/`ORPHANED`/`UNKNOWN` on
    `(git rev-parse --git-common-dir, admin-dir basename)` rather than path
    shape, which was measured misclassifying 34 of 118 worktrees (29%). The
    report also names the **excluded** set (33 of 118, six holding
    uncommitted work) and orders a nested worktree before its parent so
    removing the parent no longer strands the child's registry entry.
    Report-only: no deletions, no sentinel writes, no `--force`.
  - Companion hardening commit for the same issue (test-only, not surfaced by
    the automated changelog): `session_manager::prune`'s
    `active_workspace_paths` parameter is renamed **`in_use_workspace_paths`**
    (rename only — the old name made a `state == Active` filter read as a
    tidy-up rather than the data-loss bug it would be), a pre-existing
    zero-coverage hole in the Phase 2 `fresh_in_use` re-read is closed
    (`phase2_fresh_snapshot_spares_a_record_the_caller_set_missed`), and the
    same "deliberately unfiltered by session state" pin is extended to the
    automatic orphan-GC sweep (`reap_orphaned_worktrees`, which runs on a
    timer with `dry_run: false` hardcoded).
- read named-section PM instruction overrides from CLAUDE.md ([#4324](https://github.com/bobmatnyc/trusty-tools/pull/4324)) ([`f165b92`](https://github.com/bobmatnyc/trusty-tools/commit/f165b92733f9d1cc8656e198359f35d599154b46))
  - Grammar: `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` … `<!-- TRUSTY-MPM: WORKFLOW END -->`.
    Hosts scanned in precedence order — `CLAUDE.md` then
    `.trusty-mpm/INSTRUCTIONS.md` — with `CLAUDE.md` winning a same-section
    collision. The three floor sections (`identity`, `non-overridable-rules`,
    `framework-guaranteed-conventions`) refuse every override. Nothing here
    fails a launch: a missing/unreadable file, unclosed marker, unknown
    token, or override aimed at the floor all degrade to the bundled section
    with a `warn!`.
- peer message bus foundation — envelope, pub/sub, instance registry ([#4261](https://github.com/bobmatnyc/trusty-tools/pull/4261)) ([`6ffe001`](https://github.com/bobmatnyc/trusty-tools/commit/6ffe001de257ccb4728720d90a11abc8f59a098a))
  - Instance-bypass failure is explicit, never a silent fallback: targeting a
    dead `instance_id` returns `410 Gone` and never redirects to another live
    instance of the same definition; `404`/`409` are separately
    distinguishable.
  - Caller identity is verified, not asserted: the publish path admits only
    `kind: assistant_instance` senders (`400` otherwise), resolves the
    claimed `instance_id` against a live registration (`403
    UnregisteredSender` if not), and re-stamps `definition_id` from the
    registry rather than trusting the caller's copy.
  - Instance ids use `<definition_id>~<8 hex>` (RFC 3986 unreserved `~`, not
    DOC-60 §11's illustrative `#`, which a URI-fragment-aware client would
    truncate).
  - Deferred, not built: the durable inbox and queue-not-spawn behavior (§7),
    cross-project addressing (§12 Q4), retention policy (§12 Q1), and
    version-skew negotiation (§12 Q2/Q5).
- author PM instructions as per-section sources ([#4183](https://github.com/bobmatnyc/trusty-tools/pull/4183)) ([#4264](https://github.com/bobmatnyc/trusty-tools/pull/4264)) ([`c41735d`](https://github.com/bobmatnyc/trusty-tools/commit/c41735dcba886c3518db3b522d2614cdf7ae90a4))
  - Every existing `.trusty-mpm/` override file keeps resolving exactly as
    before. Adds a **Sprint, then Harden** workflow doctrine: sprint to
    feature-complete with targeted tests and no CI iteration loops; harden
    with the full suite, critic, and release gates before publishing; going
    fast never licenses turning red green by deleting coverage; close-and-fold
    at 3+ review rounds.
- compose the bundled-fallback PM prompt via InstructionPackage ([#4183](https://github.com/bobmatnyc/trusty-tools/pull/4183)) ([#4249](https://github.com/bobmatnyc/trusty-tools/pull/4249)) ([`eba04d6`](https://github.com/bobmatnyc/trusty-tools/commit/eba04d67ab940876f55638055a9b195cba5a3ea1))
  - Composed prompt is byte-identical to the previous four-asset-concatenation
    assembly, gated by a test that reports the first differing byte offset on
    failure. Scope is the bundled fallback only — an `AGENT_DELEGATION.md`
    override and `PM_INSTRUCTIONS_DEPLOYED.md` stay on the legacy path by
    design. Adds committed golden-prompt snapshots for both composers
    (regenerate with `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`).
- sectioned-JSON instruction package schema ([#4184](https://github.com/bobmatnyc/trusty-tools/pull/4184)) ([#4223](https://github.com/bobmatnyc/trusty-tools/pull/4223)) ([`99a1724`](https://github.com/bobmatnyc/trusty-tools/commit/99a17243866785b0b90f966631ad9d320b12b32c))
  - `validate` gates on `schema_version` before inspecting any field, so a
    package from a later schema reports "unsupported schema_version" instead
    of blaming one of its own valid new keys. Floor blocks are deliberately
    NOT constrained to canonical section order (a faithful lift of
    `BASE_PM.md` places `Trusty Tool Priority` after
    `Framework-Guaranteed Conventions`). A declared section must own at least
    one non-optional block — a section covered only by `optional` blocks
    passed validation and composed to nothing.
- observe native subagent dispatches and give delegations a real lifecycle ([#4096](https://github.com/bobmatnyc/trusty-tools/pull/4096)) ([`6f7530e`](https://github.com/bobmatnyc/trusty-tools/commit/6f7530ef650d4b7dfe1acd5b61b01bcf0c7a0136))
- hard-block git worktree add targeting /tmp, /private/tmp, /var/folders, or the scratchpad ([#3978](https://github.com/bobmatnyc/trusty-tools/pull/3978)) ([`bac23b8`](https://github.com/bobmatnyc/trusty-tools/commit/bac23b8090dfe9d6d5107133dfb7c3dfcdfb8fde))
- **Bare `tm daemon` bypassed the #2486 unsupervised-spawn guard (closes
  [#4397](https://github.com/bobmatnyc/trusty-tools/issues/4397)).** The
  guard against a launchd-managed daemon being duplicated by an unsupervised
  spawn previously existed only on the MCP-bridge path
  (`serve_stdio.rs`'s `no_spawn`); `run_daemon` (the bare-CLI path, invoked
  directly or by a launchd plist) never consulted it, so `tm daemon` run by
  hand walked straight past the same hazard — observed live as PID 35895
  serving a stale 1.2.3 image since 2026-07-29 while the on-disk binary was
  1.3.0. `run_daemon` now refuses to start when a trusty-mpm launchd unit is
  registered but this process is not launchd-supervised, unless the operator
  passes the new `tm daemon --force` opt-in.
- **Decommissioning (or soft-deleting) a session no longer leaves a phantom
  `pending_decision` behind (closes
  [#4400](https://github.com/bobmatnyc/trusty-tools/issues/4400)).**
  `supervisor_status` was reporting decommissioned sessions in
  `pending_decisions` indefinitely — two sessions that never ran sat in the
  human-confirmation queue for 19 days after being torn down. Both terminal
  paths — `decommission_with_root` and `delete_record` — now clear
  `pending_decision`/`proposed_default` unconditionally; `FleetMetrics`
  additionally filters the surfaced list to non-terminal states as defense in
  depth; and the boot-time reconcile sweep backfills (clears) the stale field
  on any terminal record persisted before this fix.
- **An unrelated legacy `.trusty-mpm/` override file no longer shadows every
  CLAUDE.md named-section override (closes
  [#4399](https://github.com/bobmatnyc/trusty-tools/issues/4399)).**
  Previously, if ANY ONE of the four replace-semantics legacy files
  (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`,
  `MEMORY.md`) was present, the whole prompt fell back to the sectionless
  legacy assembly, and every CLAUDE.md named-section override was reported
  "not applied" — including sections the legacy file had nothing to do with
  (an `AGENT_DELEGATION.md` file silently dropped a `WORKFLOW` override
  authored in `CLAUDE.md`). `WORKFLOW`, `MEMORY` and `AGENT-DELEGATION` are
  now independently addressable on that legacy path too: a named override for
  one of those sections lands unless the project's own same-section legacy
  file is what forced the branch, in which case that file still wins,
  unchanged. `IDENTITY`/`CORE`/`SEARCH` have no independent slot in the legacy
  string assembly and remain reported as unapplied, never silently dropped.
- **A corrupted bundled agent is re-deployed instead of frozen forever (closes
  [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408)).** When the
  deployed copy of a bundled agent drifts from the checksum the deploy manifest
  recorded, the file is now refreshed from the bundle rather than classified as
  a user edit and skipped. Previously a degenerate 32-byte stub that replaced
  `rust-engineer.md` in a workspace's `.claude/agents/` was unrecoverable —
  neither a redeploy nor `tm validate --repair` could reach it — so every
  session in that workspace lost the agent from its roster ("Agent type not
  found") permanently. Hand-placed and user-owned agent files are untouched.
- **`tm doctor` no longer reports a serving daemon as unreachable (issue
  #4005).** On 2026-07-26 it printed "trusty-memory unreachable at
  127.0.0.1:7070" while MCP `memory_recall` / `memory_remember` succeeded
  against that same daemon in the same minutes. Two causes, both fixed:
  the probe was single-shot with a 2 s budget, and trusty-memory's `/health`
  samples RSS/CPU behind a mutex and enumerates open file descriptors — work
  the MCP path never touches — so under load `/health` could exceed a budget
  real traffic never approached. The probe now allows 10 s, retries up to 3
  times, and above all distinguishes a TIMEOUT (which proves nothing, so it
  reports `Unknown`) from a REFUSED connection (which proves nothing is
  listening, so it still reports `Fail`). Post-restart warm-up
  (`daemon_state: "warming"`) now reports `Warn` rather than a hard failure.

- **`tm doctor` no longer reports HEALTHY while the memory daemon is wedged
  (issue #4001).** A 2xx from `/health` proves a listener accepted a socket,
  not that the daemon is doing work — which is how doctor stayed green through
  the entire #3992 incident. The memory probe now reads the daemon's own
  worker-occupancy observation out of the response body and fails on a
  reported wedge. A daemon too old to report that block is `Unknown`, not a
  pass: doctor no longer claims health it has not observed.

- **The test suite no longer writes trust-seed entries into the operator's real
  managed `.claude.json`**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): five tests
  drove the real spawn path (`ClaudeCodeAdapter::spawn`/`spawn_resume` →
  `prepare_managed_config` → `preseed_managed_trust`) without redirecting
  `$HOME`, so each one wrote a `projects.<tempdir>` entry straight into
  `~/.trusty-tools/trusty-mpm/claude-config/.claude.json` — a file that also
  holds `oauthAccount`. 2,443 of 2,538 entries in the reporting operator's
  config were `tempfile::TempDir` paths as a result. Those tests now redirect
  `$HOME` (the isolation seam `dirs::home_dir()` already honours), and a new
  regression test locks the invariant at the layer they all funnel through.
  **No production behaviour changes** — the resolver was never the defect; see
  the PR body for why the originally-reported root cause ("takes no injectable
  base") did not hold up, and why honouring an ambient `CLAUDE_CONFIG_DIR`
  would have made the leak strictly worse.
- **The managed `.claude.json` `projects` map is now self-healing**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): during the
  read-modify-write `preseed_managed_trust` already performs, entries are dropped
  when — and only when — the directory is DEFINITIVELY absent (a clean
  `NotFound`) **and** the entry carries nothing beyond the four keys the seeder
  itself writes. An ambiguous filesystem error (unmounted volume, down network
  mount, permission denied, I/O fault) always KEEPS the entry, and any entry
  carrying Claude Code's own runtime state (`lastSessionId`, `lastCost`,
  `mcpServers`, …) is preserved even when its path is gone. No new command, no
  background job.
- **A second config quarantine no longer destroys the record of the first**
  ([#4206](https://github.com/bobmatnyc/trusty-tools/issues/4206)): the two
  writers that quarantine a malformed `.claude.json` (`standalone::trust_seed`
  and `core::mcp_config`) both used the fixed name `.claude.json.corrupt`, so
  whichever ran second silently overwrote the first one's evidence. Both now
  share `trusty_common::claude_config::quarantine_path`, which timestamps the
  filename, and both log at `error!` rather than `warn!` — the workspace's
  error-capture layer filters on `!= Level::ERROR`, so the previous `warn!`
  reached no diagnostic surface at all.
- keep the PM persona on in-place relaunch and test the exec seam ([#4340](https://github.com/bobmatnyc/trusty-tools/pull/4340)) ([`714f33e`](https://github.com/bobmatnyc/trusty-tools/commit/714f33ed52f681b6d884a1c380c46935a8f2450a))
  - `compose_inplace_args` omitted `--append-system-prompt-file`; now builds
    the prompt through the same `build_prompt_file` carrier `spawn`/
    `spawn_resume` use. The `std::process::Command` construction is now a
    pure, tested `build_inplace_exec_command` — previously the last step
    before process replacement had zero coverage, which is why this could be
    reported and not refuted from the suite.
- tm ls/picker auto-prunes dead records + groups attached-active-stopped ([#4384](https://github.com/bobmatnyc/trusty-tools/pull/4384)) ([`3dfcb65`](https://github.com/bobmatnyc/trusty-tools/commit/3dfcb6575cd6f3b49929a4dccffacacf3fb52828))
  - Hardened three ways: **record-only removal** (POSTs `/decommission
    ?record_only=true`, so the sweep can never delete filesystem content),
    **two-sightings confirmation + a 5-per-call cap** (a record must read
    `unresumable` on two separate listings before it's auto-decommissioned,
    with the rest reported as "N more pending confirmation"), and a **TTY
    gate** (a scripted/piped/CI invocation of bare `tm` is a pure read, same
    as `tm ls`). `sort_sessions` now groups attached → active → everything
    else above the requested secondary order.
- stop a subagent SessionStart from clobbering claude_session_id ([#4339](https://github.com/bobmatnyc/trusty-tools/pull/4339)) ([`f634d27`](https://github.com/bobmatnyc/trusty-tools/commit/f634d27c9f11f8fb33851ee343f5dc6f26708803))
  - `correlate_session_start` now trusts only the FIRST correlation for a
    record; a later `SessionStart` with a different id is accepted only when
    the stored id's transcript no longer resolves to a live session
    (`runtime::session_id_exists`). `SessionEnd` now clears the field via
    `SessionManager::clear_claude_session_id_if`, and `mark_reactivated`
    clears it outright.
- daemon boot no longer resurrects deleted sessions; decommission no longer force-deletes dirty worktrees ([#4344](https://github.com/bobmatnyc/trusty-tools/pull/4344)) ([`114de33`](https://github.com/bobmatnyc/trusty-tools/commit/114de3331230e16b77d6c6ebd22b056037c07824))
  - `reconcile_on_boot` now asks the record's own `is_terminal()` instead of
    a hand-rolled `Decommissioned`-only check, so `Deleted` records no longer
    fall through to "gone" and get marked `Stopped`. `decommission` reuses
    the #4091 dirty-worktree check (`worktree_safety::inspect_dirt`) before
    removing an SM-created worktree; a dirty candidate is refused and
    reported, never force-deleted. Two follow-ups: the tombstone no longer
    nulls `workspace_path` when nothing was removed, and `tm sessions prune
    --state stopped` reports a distinct `decommissioned_worktree_retained`
    outcome instead of a plain `decommissioned` line either way.
- tm ls cold-latency fix ([`1c54090`](https://github.com/bobmatnyc/trusty-tools/commit/1c5409007f012516fc936e71a4deee1cd2c7972d))
  - Refs [#4322](https://github.com/bobmatnyc/trusty-tools/issues/4322) (does
    not close it — a daemon-state accumulation follow-up remains). `tm ls`
    took 4.9-9.4s cold on a 32-session fleet from blocking filesystem I/O:
    `stale_assets_for_many` ran one `spawn_blocking` with a serial loop, each
    iteration reading ~95 deployed agent/skill files per session. Two
    changes in `daemon::managed_routes::summary`: `Stopped` sessions are no
    longer asset-staleness-probed on the LIST path (56% of the I/O, for a
    marker only actionable at resume — the single-session `GET
    .../managed/{id}` path still probes everything), and the remaining
    per-session comparisons now run concurrently via `JoinSet::spawn_blocking`.
    Measured on that fleet: 3,040 → 1,140 `read_to_string` calls and 35.7 MiB
    → 15.0 MiB per listing. The `tm ls` STATE column also now renders
    `[assets ?]` for a row the daemon did not probe, via a new additive wire
    field `SessionSummary::stale_assets_unchecked`, so the absence of
    `[stale-assets]` can never be misread as "assets fresh".
- make the nested-session guard survive a cold daemon ([#4343](https://github.com/bobmatnyc/trusty-tools/pull/4343)) ([`5f8b338`](https://github.com/bobmatnyc/trusty-tools/commit/5f8b3385f68fbf622b7f2410313357502b8c476a))
  - Fixes [#4335](https://github.com/bobmatnyc/trusty-tools/issues/4335) —
    bare `tm` needing two runs after Claude Code backgrounds. Three changes:
    the managed-list endpoint takes `?slim=true` to skip the `stale_assets`
    probe for callers (folds into #4322 above), the guard's timeout goes
    2s → 8s, and the error arm now logs at debug via `guard_managed_sessions`.
- resume a dead runtime instead of switching into a bare shell ([#4304](https://github.com/bobmatnyc/trusty-tools/pull/4304)) ([`aed8014`](https://github.com/bobmatnyc/trusty-tools/commit/aed80140d4c2179bdf360791b44a218ecf1b2498))
  - `guided_resume::plan_resume` takes a third input, `runtime_live` (via the
    daemon reaper's own `session_has_live_pane` primitive), so a live tmux
    pane whose `claude` process died maps onto the existing
    `ReconcileThenRestart` recovery instead of a plain attach. The verdict is
    ANY-pane-live over the whole tmux session, not the stored `pane_id`
    alone, and fails OPEN throughout. Known limitation: a split session with
    one dead agent pane but another pane running a live non-shell command
    (`vim`, `tail -f`) still reads as live and still attaches into the idle
    pane. Because the reconcile is session-scoped (`kill_session`), the
    restart now discloses it is about to destroy the whole tmux session.
- honour the per-project worktree opt-out in both CLI paths ([#4303](https://github.com/bobmatnyc/trusty-tools/pull/4303)) ([`c11da9c`](https://github.com/bobmatnyc/trusty-tools/commit/c11da9cdaa9dc866698b0e846eb0553b8edde694))
  - `Project.worktree: false` was consulted only inside the daemon's spawn
    route; `tm launch` and the daemon-unreachable bare-`tm` fallback ignored
    it. Both now consult the registry before `ensure_base_clone`, via the new
    `project::worktree_policy` (serves daemon in-process and CLI
    out-of-process off the same `projects.json`). The `unwrap_or(true)`
    default is unchanged.
- serialise projects.json writes across processes ([#4258](https://github.com/bobmatnyc/trusty-tools/pull/4258)) ([`623d6e3`](https://github.com/bobmatnyc/trusty-tools/commit/623d6e3ee7d93bde3e1d942a79d42f60f276e93c))
  - The project-store upsert was an unsynchronised read-modify-write; two
    interleaved writers could silently drop a registration or publish a
    mangled `projects.json` (shared `.tmp` scratch path). All mutations now
    run through `ProjectStore::mutate` under a cross-process file lock,
    published atomically via `trusty_common::json_rmw`. `PATCH
    /api/v1/projects/{name}` gets the same guarantee.
- derive worktrees from git instead of walking five shapes ([#4207](https://github.com/bobmatnyc/trusty-tools/pull/4207)) ([#4224](https://github.com/bobmatnyc/trusty-tools/pull/4224)) ([`69871f1`](https://github.com/bobmatnyc/trusty-tools/commit/69871f13257846edb3109d96d5919dc33b1c851d))
  - Uses `git worktree list --porcelain` instead of five hard-coded location
    shapes. The owning checkout is now resolved with `git rev-parse
    --git-common-dir` instead of a guessed grandparent directory — the old
    guess disowned worktrees physically inside `.base/.worktrees/` but
    registered to the parent repo, making them structurally unreclaimable.
    `tm session prune --worktrees` and the daemon orphan-GC no longer propose
    a plain (non-git-registered) directory. Reclamation is now bounded by a
    structural containment rule — a candidate must be a strict descendant of
    the managed project directory that registered it — since deriving
    discovery from git widened candidates to anything registered anywhere
    under the projects root (on the dogfood machine this admitted five of
    the operator's own long-lived checkouts). A git-`locked` worktree is
    never enumerated for reclamation (the removal path previously treated a
    lock refusal as a generic failure and fell back to deleting the
    directory outright, defeating the lock).
- safe deregister on DELETE /indexes/:id + restore-derived stages on POST /indexes ([#4218](https://github.com/bobmatnyc/trusty-tools/pull/4218)) ([`59674a0`](https://github.com/bobmatnyc/trusty-tools/commit/59674a07d5b8fb74db4374d32dddaf9ae3674921))
- stop two paths writing into a destroyed worktree ([#4204](https://github.com/bobmatnyc/trusty-tools/pull/4204)) ([#4209](https://github.com/bobmatnyc/trusty-tools/pull/4209)) ([`7973aa5`](https://github.com/bobmatnyc/trusty-tools/commit/7973aa5a94c5418ed836578a1621a5e09b1e09a5))
- deploy launch/connect/meta-launch agents into a tier --setting-sources loads ([#4205](https://github.com/bobmatnyc/trusty-tools/pull/4205)) ([`5a68695`](https://github.com/bobmatnyc/trusty-tools/commit/5a686959b360f6b1f12c0e79b6d59fb1b4c2bacd))
- deliver the computed agent roster to the PM prompt ([#4196](https://github.com/bobmatnyc/trusty-tools/pull/4196)) ([`5bf231b`](https://github.com/bobmatnyc/trusty-tools/commit/5bf231b9d085c113bb2b1f48480324023d49475e))
- correct two wrong instructions in bundled assets (index_id, ticket routing) ([`f2b79b3`](https://github.com/bobmatnyc/trusty-tools/commit/f2b79b3add708dd31b9f88b2494aa9891ec49ebc))
- refuse cross-branch git push from managed worktrees ([#2867](https://github.com/bobmatnyc/trusty-tools/pull/2867)) ([#4090](https://github.com/bobmatnyc/trusty-tools/pull/4090)) ([`8d89ca1`](https://github.com/bobmatnyc/trusty-tools/commit/8d89ca1c6d7476a2f7aaa1687c427d48a5dd4513))
- refuse to reclaim a worktree holding unsaved work ([#4091](https://github.com/bobmatnyc/trusty-tools/pull/4091)) ([`dc6bc2a`](https://github.com/bobmatnyc/trusty-tools/commit/dc6bc2a844c7502a03cdaa552f2ab5caddf354d2))
- isolate $HOME in every test that seeds ~/.claude.json ([#4120](https://github.com/bobmatnyc/trusty-tools/pull/4120)) ([`898ec26`](https://github.com/bobmatnyc/trusty-tools/commit/898ec260f397b5915dd8805c6ca2bd2ecd5106bf))
- stream Bedrock via ConverseStream ([#3767](https://github.com/bobmatnyc/trusty-tools/pull/3767)) ([#4112](https://github.com/bobmatnyc/trusty-tools/pull/4112)) ([`49dd042`](https://github.com/bobmatnyc/trusty-tools/commit/49dd042ae5727c1f0e70d8008279e8006e714123))
- stop bare tm from misreporting a managed pane as 'not a git project' ([#4061](https://github.com/bobmatnyc/trusty-tools/pull/4061)) ([#4065](https://github.com/bobmatnyc/trusty-tools/pull/4065)) ([`6c868e9`](https://github.com/bobmatnyc/trusty-tools/commit/6c868e99f1c62bad2078666a31d23aeae0b55a4b))
- serialize concurrent ~/.claude.json trust seeds ([#4075](https://github.com/bobmatnyc/trusty-tools/pull/4075)) ([`707a09d`](https://github.com/bobmatnyc/trusty-tools/commit/707a09dc631cad17a7fbc754f659d8de45a6f1c6))
- emit ChatEvent::Error on failed SSE streams and restore streaming sampling parity ([#3980](https://github.com/bobmatnyc/trusty-tools/pull/3980)) ([`207fa8d`](https://github.com/bobmatnyc/trusty-tools/commit/207fa8de54ddb0361bf6aecc53c079096a5b5f72))
- scan .claude/worktrees for orphans + stop .gitignore fallthrough ([#3974](https://github.com/bobmatnyc/trusty-tools/pull/3974)) ([`cb299bb`](https://github.com/bobmatnyc/trusty-tools/commit/cb299bba75a0c9ff66bbc2719571ce2991482047))
- gate MCP trust-set membership on actual pin success, not the toggle (fifth instance) ([#3951](https://github.com/bobmatnyc/trusty-tools/pull/3951)) ([`58fcca6`](https://github.com/bobmatnyc/trusty-tools/commit/58fcca684a51528dac2801e669f437917e27ee21))
- gate trusty-memory/trusty-search trust on the manifest toggle actually being on ([#3946](https://github.com/bobmatnyc/trusty-tools/pull/3946)) ([`df51741`](https://github.com/bobmatnyc/trusty-tools/commit/df5174178f7084d830e4d23d1ca41bfe0016b471))
- tm launch no longer auto-trusts every committed .mcp.json MCP server ([#3940](https://github.com/bobmatnyc/trusty-tools/pull/3940)) ([`88fc6f4`](https://github.com/bobmatnyc/trusty-tools/commit/88fc6f49301ed3fd83e876021e07b3339a9080d6))
- daemon-managed sessions now trust the project's own trusty-mpm MCP server ([#3924](https://github.com/bobmatnyc/trusty-tools/pull/3924)) ([`385aeff`](https://github.com/bobmatnyc/trusty-tools/commit/385aeff78cf4cac00c3ecd0207ce0d24af2ae0d3))
- pause/resume skills load deferred MCP tool before calling it ([#3919](https://github.com/bobmatnyc/trusty-tools/pull/3919)) ([`39451cf`](https://github.com/bobmatnyc/trusty-tools/commit/39451cf69b8dfef81e7bddb19532f17f09d6c99b))

### Changed

- per-PR changelog entries are now fragment files, not edits to a shared
  `## [Unreleased]` section (closes [#4476](https://github.com/bobmatnyc/trusty-tools/issues/4476))
  - Every PR that changes `crates/<crate>/src/**` adds
    `crates/<crate>/changelog.d/<issue-or-pr-number>-<slug>.md` — line 1 the
    category, the rest the bullet. Distinct filenames per PR mean git never sees
    a conflict; the shared section guaranteed one. Five concurrent trusty-mpm PRs
    on 2026-07-31 each wrote into `## [Unreleased]` and every merge forced the
    next to rebase and hand-resolve it.
  - `scripts/assemble-changelog.sh` folds a crate's fragments into a
    `## [<version>]` section at release time and deletes them in the same
    operation; `scripts/bump-version.sh` calls it (with a `--check` pre-flight
    before `Cargo.toml` is touched, so a changelog problem no longer leaves the
    repo half-bumped).
  - New CI gate `scripts/check_changelog_fragment.sh` fails a PR that changes
    crate source without recording it. The rule was review-gate-only before.
  - `scripts/generate-changelog.sh` is deleted. Its `git cliff --unreleased
    --prepend` blindly stacked a second `## [Unreleased]` heading — the defect
    [#2793](https://github.com/bobmatnyc/trusty-tools/issues/2793) tracks — so
    the `bump-version.sh` stopgap guard for that is gone too. `cliff.toml`
    remains, scoped to the GitHub Release body only.
  - The whole workspace is migrated in the same change: 22 crates' pending
    `## [Unreleased]` sections became fragments, 4 stale buried `[Unreleased]`
    headings left by #2793 were removed without moving their text, and
    `trusty-kb`/`trusty-bm25-daemon` gained the `---` separator the assembler
    needs. Every crate's `--check` pre-flight passes; word-level conservation
    confirms nothing was dropped.
  - An empty `changelog.d/` is explicitly NOT an error — it is the steady state
    after a release. A tracked `changelog.d/README.md` keeps the directory
    present so the next PR still sees a fragments project.
  - The gate asks the real assembler whether it would accept a crate's
    fragments rather than checking that a file exists, so a 0-byte, bodyless,
    mis-categorised or nested fragment fails in the PR that wrote it instead of
    silently vanishing from the release. `Removed` and `Security` join the
    category set, which the hand-written sections already used.
- bundled agents deploy to the tm-managed user tier, never per-workspace and never to `~/.claude` (closes [#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409))
  - `FrameworkPaths::agent_deploy_dir()` is the single destination for every
    bundled-agent deploy, validate, staleness, reset, and doctor call site:
    `$CLAUDE_CONFIG_DIR/agents` (`~/.trusty-tools/trusty-mpm/claude-config/agents`).
    Unlike `claude_agents_dir()` it is never rewritten project-local by
    `for_managed_project`/`for_managed_workspace`.
  - `tm install` no longer writes composed agents into the operator's generic
    `~/.claude/agents/` — the highest-severity breach on the issue, since that
    directory belongs to a Claude Code install with nothing to do with
    trusty-mpm.
  - Session launch, `tm sessions sync-assets`, and `tm catalog apply` no longer
    deploy bundled agents into a workspace's `.claude/agents/`. That directory
    is now reserved for hand-placed (and future project-custom) agents, which
    are still never touched.
  - Session launch and sync-assets additionally RETRACT the bundled agents an
    older binary deployed into a workspace. The project tier outranks the
    config-dir tier in agent resolution, so a stale copy left behind would
    shadow the canonical roster permanently and would no longer be refreshed by
    any deploy — the #4408 shadowing incident made permanent. Retraction removes
    only manifest-tracked, framework-owned files; hand-placed and user-owned
    files survive byte-identical, and a corrupt manifest aborts the retraction
    rather than guessing.
  - `tm doctor`'s `agents` and `agent_skills` probes, `tm agent list`/`tm agent
    show`, and the deployment-completeness gate follow the roster to the new
    tier. Probing the workspace tier after the flip would have reported every
    healthy install as broken and driven the spawn/resume gate into a permanent
    repair loop; `tm agent list` would have reported an empty roster.
  - `tm install --reset-agents --reset-agents-workspaces` now RETRACTS a
    workspace's bundled agents instead of force-recomposing them. Recomposing
    was correct while workspaces were a deploy destination; after the flip it
    re-created, inside live sessions' workspaces, exactly the shadow this change
    removes. The `--reset-agents <names>` scope is still honored, and removals
    are reported on their own line.
  - `tm repair deploy` follows the agent ledger to the new tier and actually
    removes the scratch files it reports. It previously derived a "base" path
    from each `*.tmp` orphan and re-derived the old fixed `<name>.tmp` from it,
    which never matches the per-process scratch names below — so it unlinked
    nothing while still printing every orphan as removed. It now unlinks the
    path it found and reports only what it actually deleted.
  - EVERY writer of the shared agent ledger — session launch, sync-assets,
    retraction, `tm install --reset-agents`, and `tm catalog apply --prune` —
    now performs its read-modify-write under the lock below. `reset_agents` and
    `prune_agents` were the two that ran unlocked against the shared directory,
    which is the highest-traffic race with a concurrent session launch.
  - The agent deploy ledger's read-modify-write is now serialised across
    processes by an advisory lock on a `.trusty-mpm-manifest.json.lock` sidecar,
    and every atomic write stages through a per-process, per-attempt temp name
    instead of a fixed `.tmp` sibling. Both were survivable while each workspace
    had its own deploy directory; against ONE machine-global directory shared by
    every concurrent session launch, sync-assets run, and `tm catalog apply`,
    the fixed temp name lets two writers publish torn JSON and the unlocked
    load-modify-write silently drops one writer's entries — after which the
    files those entries described are treated as untracked and frozen, which is
    #4408's failure shape reached by a race.
  - Known consequences of a machine-global agent tier, neither solved here:
    - A per-project `[agents] exclude` no longer removes an agent from a
      session's view — it only refrains from writing it, and a sibling project
      that selects the agent still puts it there.
    - `tm catalog apply --prune` now deletes from the one directory EVERY live
      session reads, mid-session, driven by the daemon-wide baseline manifest.
      Before the flip it pruned `~/.claude/agents`, which no managed session
      read, so the blast radius was effectively nil. `--prune` remains opt-in
      and still only removes manifest-tracked managed files, but an operator
      running it now affects every running session, not one workspace.
- **`core::doctor::CheckStatus` gained an `Unknown` variant (issues #4005,
  #4001).** This is a BREAKING change for any downstream exhaustive `match`
  over the enum (E0004) and needs a MINOR bump, not a patch — the same trap
  that forced the `trusty-analyze` 0.7.3 yank. Four in-workspace match sites
  (Slack + Telegram formatters, the `tm doctor` CLI renderer) gained an arm.
  `Unknown` ranks above `Warn` and below `Fail`, so a single indeterminate
  check can never leave the aggregate report reading healthy.
- bump to 1.2.3 ([#4229](https://github.com/bobmatnyc/trusty-tools/pull/4229)) ([`7c4f056`](https://github.com/bobmatnyc/trusty-tools/commit/7c4f05643508bd9ad04b9cf5ba77dead3cb4cfcb))

### Removed

- `core::agent_reset::reset_project_agents` — force-recomposed the bundled roster into an arbitrary directory, narrowed by a project's harness roster ([#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)). The workspace sweep was its only caller and now retracts instead, leaving it with zero production callers. A public function that writes the bundled roster into a workspace is exactly the shadow-creating footgun this change removes, so it is deleted rather than left callable. `ResetResult::deselected` is retained for the report's shape but is no longer populated by anything.

### Documentation

- add a plain-speaking prose rule to the PM instructions ([#4316](https://github.com/bobmatnyc/trusty-tools/pull/4316)) ([`899a949`](https://github.com/bobmatnyc/trusty-tools/commit/899a949aaee9776010ecfb7acfa3c087d195ce3a))
- fix stale `.invalid` fixture-convention pointer ([#4308](https://github.com/bobmatnyc/trusty-tools/pull/4308)) ([`098ab27`](https://github.com/bobmatnyc/trusty-tools/commit/098ab270a0580f0849a53f64e44b958878475ede))

## [1.2.3] — 2026-07-28

PATCH: merge-and-local-install only, no crates.io publish.

### Fixed

- **`tm launch`, `tm connect`, and `tm meta launch` deployed agents to a tier
  `--setting-sources` excludes, so no tm-deployed agent ever loaded**
  ([#4203](https://github.com/bobmatnyc/trusty-tools/issues/4203)): all three
  CLI paths built their `FrameworkPaths` with `default()`, which resolves
  against `dirs::home_dir()` and writes the agent roster into
  `$HOME/.claude/agents` — the `user` tier. All three then spawned `claude`
  carrying `--setting-sources project,local`, a list that deliberately excludes
  `user` (#1269). The deployed tier and the loaded tiers never intersected, so
  every agent the launch step had just deployed was invisible to the session it
  was deployed for, and nothing reported it: the deploy genuinely succeeded and
  the load silently found nothing.

  These paths now deploy through a new `session_launch::prepare_isolated_session`
  entry point, which resolves the layout itself rather than accepting one from
  the caller — the harness cwd is the deploy base, so `<cwd>/.claude/{agents,skills}`
  lands in a tier the `project`/`local` sources read. Removing the parameter
  removes the defect: there is no longer a wrong `FrameworkPaths` for a call
  site to pass. This is the same correction the daemon's `spawn_managed_inproject`
  already carried (#1931); the three CLI paths never received it. `tm session
  start` is deliberately unchanged — it spawns a bare `claude` with no
  `--setting-sources`, so it does read the user tier and `default()` is correct
  there.

  **Operator-visible change:** `tm connect` now writes the agent roster, skills,
  and output styles into the `.claude/` directory of your **live checkout**
  rather than `$HOME/.claude/`, so they will show up in `git status` for that
  repo. The scaffolding paths are added to `.gitignore` automatically (#3427),
  and existing hand-placed or committed `.claude/` content is never overwritten
  — unmanaged files are skipped as user-owned, and managed files whose checksum
  drifted are skipped with a warning.

- **Two paths no longer write into a destroyed worktree**
  ([#4204](https://github.com/bobmatnyc/trusty-tools/issues/4204)):
  `tm install --reset-agents --reset-agents-workspaces` and the bare-`tm`
  in-place relaunch both gated on `workspace_path.is_dir()` alone, which a
  gutted worktree — `.git` and source tree stripped, directory node surviving —
  passes. The sweep recomposed 45 agent files into such a husk and the relaunch
  exec'd `claude` into it. Both now share one predicate,
  `core::workspace_liveness::workspace_liveness`, which skips ONLY on positive
  evidence of death: it accepts `.git` as a FILE (the `gitdir:` pointer every
  linked worktree carries, so no legitimate managed workspace is refused),
  requires a `.git` only where one is owed (a tm-provisioned workspace or a
  `.worktrees/<id>` path), and — at EVERY stat it performs, including the
  initial existence check — reports a filesystem it could not interrogate as
  `Indeterminate`, explicitly not dead, so a permission failure, a symlink
  loop, or a stalled network mount can never silently stop servicing a live
  workspace. A gutted workspace, and a session record whose workspace has
  vanished, are both now reported to the operator with a skip reason rather
  than dropped silently.

- **Session index GC would have silently stopped reclaiming trusty-search disk**
  ([#4123](https://github.com/bobmatnyc/trusty-tools/issues/4123)):
  trusty-search's `DELETE /indexes/:id` now preserves on-disk data unless
  `?delete_data=true` is passed. Both index-GC call sites in `search_gc.rs` —
  decommission teardown and the periodic orphan sweep — now opt in explicitly.
  Without this they would have kept removing the registration while leaving the
  index data behind forever: the workspace each one describes is a disposable
  worktree that is about to be deleted, so nothing will ever re-register at
  that root and the preserved data would be unreachable garbage. The same PR
  ([#4218](https://github.com/bobmatnyc/trusty-tools/pull/4218)) also closes
  [#4110](https://github.com/bobmatnyc/trusty-tools/issues/4110) — see the
  bundled-fixes entry below for that half of the change.

- **Bundled from `main`: trusty-search P0 corpus quarantine + stage-status fix**
  ([#4122](https://github.com/bobmatnyc/trusty-tools/issues/4122),
  [#4110](https://github.com/bobmatnyc/trusty-tools/issues/4110)): this
  release is merge-and-local-install only (no crates.io publish), so a local
  install built from this commit also picks up two trusty-search fixes that
  landed on `main` alongside this bump. Neither touches trusty-mpm's own
  source; both are called out here because this version bump is the vehicle
  that carries them to the operator's next restart.

  An index whose durable corpus failed to open at load time is now
  write-quarantined (#4122): ordinary incremental writes (watcher, boot-time
  reconciler, `POST /indexes/{id}/index-file`) now refuse rather than silently
  building a fresh, partial corpus over the never-opened original — the shape
  that took one production index from 0 to 1,334 chunks of wrong content
  before coming back "healthy" on the next restart. The quarantine lifts
  automatically once a subsequent restart's `CorpusStore::open` succeeds.

  `POST /indexes/:id` no longer discards the outcome of the restore it just
  performed (#4110): `search_capabilities` is now derived from the actual
  post-restore stages via `derive_warm_boot_stages` (the same classifier
  warm-boot and lazy-restore already use), so a fully intact index no longer
  comes back advertising no vector lane — previously that meant semantic
  search hard-errored and `search_all` silently degraded to BM25-only until
  the next daemon restart.

---
## [1.2.0] — 2026-07-27

MINOR: substantial new public library surface (`core::push_guard`,
`core::claude_json_guard`, `control::delegation_tracker`,
`core::worktree_safety`) plus the MCP-trust hardening chain. Two internal
plumbing functions (`core::mcp_config::managed_mcp_server_names`,
`core::standalone::trust_seed::preseed_managed_trust`) did gain parameters,
which is technically MAJOR-shaped for a 1.x crate; no published crate depends
on `trusty-mpm` as a library (verified against the published `trusty-code`
0.2.0 and `trusty-agents-common` 0.3.0 manifests, neither of which declares
it), and no in-workspace caller outside this crate references either symbol,
so a 2.0.0 would signal a break that cannot reach anyone.

### Added

- **Cross-branch `git push` guard for every worktree of a managed base clone**
  ([#2867](https://github.com/bobmatnyc/trusty-tools/issues/2867)): a bundled
  `pre-push` hook now refuses any push that would OVERWRITE AN EXISTING remote
  branch with the history of a branch of a DIFFERENT name — the shape that
  clobbered PR #2863's reviewed lineage. One rule, applied identically whether
  `HEAD` is attached or detached: updating an existing branch requires the
  source branch name to equal the destination branch name. It is installed into
  `$GIT_COMMON_DIR/hooks` at base-clone time, which every worktree of that base
  shares, so it also covers ad-hoc `git worktree add` worktrees an agent
  creates for itself (the actual incident path, which no other trusty-mpm code
  path ever sees). Three shapes are deliberately out of scope and pass through:
  non-branch destinations (tags, notes); **creating** a branch that does not
  exist on the remote yet (git reports an all-zero `<remote sha>`; there is no
  lineage to destroy, and `git push origin <sha>:refs/heads/<new-name>` is the
  standard way to rescue work out of a detached worktree); and remote-branch
  deletes — not because they cannot happen by accident, but because a deleted
  GitHub branch is recoverable (the PR retains its commits at
  `refs/pull/<N>/head`) whereas a force-clobbered lineage is not, post-merge
  cleanup is routine, and server-side branch protection is the right layer to
  police deletion. Note that a configured `remote.<name>.push` refspec makes a
  **bare** `git push` land on an arbitrary branch — even from a detached
  `HEAD`, where `push.default` is never consulted — which is why the exemption
  is keyed on create-vs-update rather than on `HEAD` state or on how the push
  was spelled.
  Installation refuses rather than overwrites whenever the existing `pre-push`
  is not provably ours — a foreign hook, a symlink, a `core.hooksPath`
  redirect, or a file that cannot be read at all (invalid UTF-8, a permissions
  error) — so it never fights husky/lefthook, and a hook that cannot be
  inspected is never destroyed. The write is atomic (temp file + `rename`)
  because that one file is executed by every worktree of the clone. Set
  `TM_ALLOW_CROSS_BRANCH_PUSH=1` for a deliberate cross-branch push.

- **`tm repair push-guard` retrofits that guard onto an already-provisioned
  clone, and `tm doctor` reports when one is missing**
  ([#2867](https://github.com/bobmatnyc/trusty-tools/issues/2867)): the guard
  installs automatically only on the CLONE path, so every base clone that
  existed before it shipped — exactly the old, many-worktree clones the
  incident happened in — was silently unprotected with no supported way to fix
  that. `tm doctor` now carries a `push_guard` check that warns when the
  current project's clone has no guard (or an older revision) and names the
  retrofit command; `tm repair push-guard` performs it. The retrofit is
  idempotent, writes into the clone's shared hooks directory so ONE invocation
  from ANY worktree covers them all with no git config write, honours the same
  never-overwrite-a-foreign-hook refusal, and offers `--dry-run` to report
  without touching a file that live sessions are executing.

- **Agent delegations are now tracked automatically, with a real lifecycle**
  ([#2864](https://github.com/bobmatnyc/trusty-tools/issues/2864), slices
  S1+S2): the daemon observes every native subagent dispatch from the
  `PreToolUse` hook it already receives on every managed session, so tracking no
  longer depends on the PM voluntarily calling `agent_delegate`. Delegations move
  `Running` → `Completed`/`Failed` instead of sitting in `Queued` forever, and a
  `Delegation` now carries `source`, `tool_use_id`, `agent_id`,
  `transcript_path`, `cwd`, `started_at`, and `ended_at`. A dispatch that both
  declares (`agent_delegate`) and executes the *same* task produces one record,
  not two.
- **`tm hook` forwards Claude Code's subagent correlation keys** (#2864):
  `tool_use_id` and `transcript_path` on tool events, and `agent_id` /
  `agent_type` / `agent_transcript_path` on `SubagentStop`. A `tool_response` is
  relayed only for a subagent-dispatch tool and only as a fixed five-key
  projection, so an ordinary tool's output is never forwarded. All fields are
  additive.

- **`tm hook --pm-guard` now hard-blocks `git worktree add` targeting `/tmp`,
  `/private/tmp`, `/var/folders`, `$TMPDIR`, or the harness scratchpad**
  (worktree-hygiene follow-up to
  [#3955](https://github.com/bobmatnyc/trusty-tools/issues/3955)): worktrees
  provisioned there silently fail unrelated tests (trusty-search's
  `SENSITIVE_PATH_PREFIXES` denylist) and can be reaped mid-task by the OS or
  harness. The check is called directly from `pm_guard()` BEFORE Guard 4's
  native-subagent-dispatch exemption, and is unconditional — including for
  Task/Agent-dispatched subagents, which is who actually provisions these
  worktrees in practice. `pm_guard_bash::evaluate_worktree_add_command`
  resolves the target (relative paths, `cd`/`git -C` tracking, `~`/`$TMPDIR`/
  `$TMP` expansion, lexical `.`/`..` normalization) without touching the
  filesystem; only the exact `worktree add` subcommand is affected —
  `list`/`remove`/`prune`/`lock`/… and ordinary temp usage (`mktemp`, temp
  files, cargo build artifacts) are untouched.

  **Round 2 (code-critic BLOCK, PR #3978):** the guard also pierces Guard 1
  (`CLAUDE_MPM_SUB_AGENT`) — the automatic marker trusty-agents' own
  subprocess/runner spawn helpers stamp on every nested subagent process
  (`crates/trusty-agents/src/subprocess/spawn.rs:189-192`,
  `crates/trusty-agents/src/agents/claude_code_runner/run.rs:211-215`), which
  is who actually provisions most of these worktrees. Before this round,
  Guard 1 short-circuited to ALLOW before the stdin payload was even read, so
  a `CLAUDE_MPM_SUB_AGENT=1`-tagged `git worktree add /tmp/...` call was
  silently allowed. Guards 2/3 (`TRUSTY_MPM_DISABLE_HOOKS` /
  `TRUSTY_MPM_PM_UNRESTRICTED`) are deliberately left UN-pierced — neither is
  ever set programmatically anywhere in this codebase's Rust source, so
  whoever sets them still gets exactly that, including for this guard.
  **Round 3 correction:** that is narrower than "operator-only" — Claude
  Code's `settings.json` `env` object can set either var for every hook
  invocation (live-reloaded mid-session), and a write to
  `.claude/settings.json` is not itself blocked by `pm_guard` today, so the
  PM or an exempt subagent can self-exempt from this entire guard via that
  pre-existing, unrelated gap. Not introduced or widened by this change; not
  fixed here; tracked as
  [#3981](https://github.com/bobmatnyc/trusty-tools/issues/3981).

### Changed

- `trusty-common` requirement raised to `^0.27` (was `^0.26`): 0.27.0 makes
  `ChatEvent` `#[non_exhaustive]`, which a `^0.26` requirement cannot express.
  `activity::monitor`'s `ChatEvent` match gained a wildcard arm accordingly.

- **Orphan-sweep candidates are processed in sorted order** rather than
  filesystem order, so a `--dry-run` preview predicts the real run and any
  ordering-dependent behaviour is reproducible.

### Fixed

- **Two wrong instructions corrected in the bundled assets composed into every
  session's prompt.** Both were static text that no project could override, and
  both had demonstrably misrouted live sessions.

  *Code search.* Agents were told to pass an `index_id` that
  does not exist: `BASE_PM.md` instructed "Always pass `index_id` = the
  project directory name (e.g. `index_id: "trusty-mpm"`)", and the
  `tm-tool-usage-guide` / `tm-circuit-breaker` skills repeated it in their
  worked examples. No index is registered under a project *name*, so the call
  returned `404 unknown index` every time — agents concluded code search was
  broken and silently degraded to `grep`. It also defeated the fix it sat on
  top of: the launcher already pins each session to its own index via
  `trusty-search serve --index <id>` in `.mcp.json` precisely so a bare call
  cannot reach the wrong project (issue #1373), and the tool schema documents
  `index_id` as defaulting to that pinned index when omitted. All four sites
  now instruct OMITTING `index_id`, and the misleading worked examples are
  gone. Deliberately no id *format* is documented, so the guidance stays
  correct however ids are derived. Because `BASE_PM.md` is the last,
  non-overridable instruction layer, no project could correct this locally —
  it affected every session on every project.

  *Ticket routing.* Issue bookkeeping was routed to the `version-control`
  agent instead of `ticketing`. `PM_INSTRUCTIONS.md` sent
  `gh issue list/view/create/close` (prohibition P6) and all "ticket
  references" to Version Control; `tm-bug-reporting` sent manual issue filing
  there too; and `tm-ticketing` actively argued for the split, on the premise
  that the `ticketing` agent only spoke `mcp-ticketer`/`aitrackdown` and so
  could not touch GitHub. That premise is false — `ticketing` is granted the
  full tool set and uses `gh` directly — so the rationale was stale, not a
  competing design. One line is now drawn consistently across all four assets:
  issue/ticket **bookkeeping** (create, update, close, label, triage, comment)
  is `ticketing`; **git and PR mechanics** (branch, push, rebase, conflict
  resolution, merge, release, tag) stay `version-control`; opening or editing
  a PR *body* is bookkeeping, pushing or merging it is not. `P6` is split into
  `P6` (issues → Ticketing) and `P7` (PRs/git → Version Control), and the
  bundled `ticketing` agent now documents GitHub as a first-class backend
  alongside the other two, with GitHub-native ids (`#4069`) and states
  (open/closed). `AGENT_DELEGATION.md` carries the same defect and is
  deliberately untouched here — its content diff is owned by the delegation
  roster work, which replaces that section wholesale rather than hand-editing
  it.

- **The PM prompt now advertises the agents that are actually deployed**
  ([#4069](https://github.com/bobmatnyc/trusty-tools/issues/4069)): the
  delegation roster was computed at every launch (`build_instructions` →
  `generate_authority`) but never reached the delivered prompt —
  `resolve_pm_prompt` fell back to the static `AGENT_DELEGATION.md` asset, whose
  hand-maintained table has named the same 8 agents since 2026-07-03. The
  bundled roster carries 37 delegatable agents, so `ticketing` and
  `memory-manager` appeared **zero times** in the delivered prompt and the PM
  could not route to them. The bundled default now appends a live
  `## Delegation Authority` roster scanned from the tiers a session loads agents
  from — `<project>/.claude/agents`, the managed `CLAUDE_CONFIG_DIR/agents`, and
  `~/.claude/agents`, deduped case-insensitively with project-tier precedence.
  The bundled routing doctrine (make/mise routing, keyword routing, the ops
  table) is appended to, never replaced, with a note between the two declaring
  the roster authoritative on WHICH agents exist and instructing the PM to
  re-route rather than retry if a listed agent turns out not to be loadable in
  the current launch mode. An explicit project/user `AGENT_DELEGATION.md`
  override still replaces the whole section exactly as documented. A tier that
  cannot be enumerated (permissions, bad mount) now warns instead of being
  silently indistinguishable from an empty one.

- **Three bundled agents were invisible to every delegation roster**
  ([#4069](https://github.com/bobmatnyc/trusty-tools/issues/4069)):
  `memory-manager`, `mpm-agent-manager`, and `mpm-skills-manager` declared
  `role: base` in their own frontmatter despite being ordinary delegatable
  agents. `scan_agents` drops any role beginning with `base` — a deliberate rule
  that catches foundation agents whose file name does not follow the `BASE-`
  convention — so all three were silently discarded from every tier they deploy
  into, on every launch path. Their `role` is now their own name; `extends:
  base-agent` already carried the inheritance. A new asset-level invariant test
  asserts every non-`BASE-*` bundled agent survives the scan, so a future
  misfiling fails CI rather than silently shrinking the roster.

- **The worktree reclaim path no longer force-deletes uncommitted work**
  ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091)): the
  orphaned-worktree sweep had no dirty-tree check anywhere — its entire safety
  model was the #3649 ownership sentinel, and a candidate with a KNOWN owner
  that passed the owner-terminal and grace-window gates was removed with
  `git worktree remove --force` (falling back to `fs::remove_dir_all`) no
  matter what was in it. Because `session_context_pause` prunes by DEFAULT,
  this fired on an ordinary `/tm-session-pause`. A new
  `session_manager::worktree_safety::inspect_dirt` gate now runs on every
  candidate the ownership gate approves, and refuses to remove a worktree that
  has modified/staged tracked files, untracked (non-ignored) files, or commits
  present on no remote — that last case matters because
  `remove_session_worktree` also runs `git branch -D session/<leaf>`, so an
  unpushed commit loses its last reachable ref. **Every error path resolves to
  DIRTY**: a git subprocess that cannot run, exits non-zero, or returns an
  unparsable count, and an unreadable directory, all skip rather than delete.
  Skips are never silent — they are returned as
  `skipped_dirty` (path, reason, file/commit counts) from the sweep, as
  `skipped_dirty_worktrees` from the `session_context_pause` MCP tool, in the
  `prune-worktrees` HTTP response, printed by `tm session prune-worktrees`, and
  warned per-path by the daemon's orphan-GC loop. Discarding that work requires
  an explicit opt-in (`discard_dirty` on the HTTP route,
  `tm session prune-worktrees --discard-dirty`); `--force` alone does NOT imply
  it, and the pause tool's schema is closed with no such knob at all, so no
  default `/tm-session-pause` can reach it. Purely additive — the #3649
  owner/sentinel guard is unchanged.

- **The dirty-tree guard now sees work `git status` hides**
  ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091), review
  round): `git status --porcelain` reports what git has been *configured* to
  show, and the most valuable content in a session worktree was outside that.
  Six holes closed, each with a test:
  - **Nested repositories are now enumerated, not inferred.** A worktree
    containing nested git worktrees or clones that hold uncommitted or unpushed
    work is DIRTY regardless of gitignore — the `.claude/worktrees/` agent shape
    is gitignored on `main`, so seven live nested worktrees inside one session
    checkout produced `dirty_files = 0`. Since the #3649 gate deliberately
    refuses to delete those nested worktrees directly, the sweep would have
    honoured that refusal and then deleted their parent. Registered worktrees
    come from `git worktree list --porcelain`; unregistered clones come from a
    bounded walk of gitignored subtrees that skips regenerable build directories
    (`target/`, `node_modules/`, …). A nested worktree that is itself clean and
    pushed does NOT pin its parent.
  - **`.trusty-mpm/` is no longer excused wholesale.** Only
    `scrollback.txt` and `last-instructions.md` are treated as disposable;
    pause snapshots under `sessions/`, checkpoints, and anything else — a live
    example held twelve files of hand-rescued uncommitted Rust source — now
    count as work, whether the project gitignores the subtree or not.
  - **Config can no longer silence the guard.** `status.showUntrackedFiles=no`
    made `git status --porcelain` exit 0 with empty output while untracked work
    sat on disk, and a `core.excludesFile` naming that work hid it the same way
    — either one in the shared `.base/.git/config` would have blinded every
    worktree in a sweep. Both are now pinned on the command line.
  - **`GIT_DIR`/`GIT_WORK_TREE` and friends are stripped** from every git
    invocation, so an inherited environment cannot point the check at a
    different (clean) repository.
  - **Unpushed commits on `session/<leaf>` are counted**, not just on `HEAD`.
    `decommission` force-deletes that branch by directory name, and a worktree
    switched off its own branch reported HEAD fully pushed while the branch
    about to be destroyed was not.
  - **The verdict is re-checked immediately before each removal.** Computing it
    once up front left the whole sweep's duration between "certified clean" and
    "deleted".

- **A nested self-contained clone is no longer assessed with the wrong loss
  model** ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091),
  review round 3): the nested-repository scan found unregistered clones in
  gitignored subtrees and then asked them the *candidate's* questions — `HEAD`
  and `session/<leaf>` — which are the right questions only when the object
  store lives outside the candidate and survives it. A self-contained clone
  keeps its entire `.git` INSIDE the candidate, so every local branch, tag,
  stash entry and reflog dies with it. A clone whose `HEAD` sat on a pushed
  `main` while a `feature` branch held the only copy of a commit answered clean
  on all three questions and was destroyed. The two shapes are now separated by
  `git rev-parse --path-format=absolute --git-common-dir` — a direct test of
  *does this repository's object store live inside the directory we are about
  to delete* — and a self-contained clone is measured with
  `rev-list --count --all --not --remotes` plus a `refs/stash` check. Registered
  nested worktrees keep the shared-store model, so a clean agent worktree still
  does not pin its parent. A clone with no remotes at all counts every commit,
  which is the correct answer for a scratch clone nobody pushed.

- **`.trusty-mpm/`'s existence probe no longer fails toward CLEAN**
  ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091), review round
  3): it used `Path::exists()`, which collapses every I/O error to `false`,
  while the first status pass had already skipped every `.trusty-mpm/`-scoped
  entry on the promise that the second pass owned them — so a permission error
  became CLEAN, inverting the module's own fail-safe invariant.

- **The unpushed-commit check now covers the bare branch spelling too**
  ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091), review round
  3): `core::worktree_naming::worktree_branch_for` says `session/<leaf>`, but
  `provisioner/workspace.rs` names the branch for the
  `.base/.worktrees/<session-id>` shape **bare** — the dominant population of a
  sweep — so the check was inert exactly where most worktrees live. Both
  spellings are now counted, in one `rev-list` so a shared commit is not counted
  twice.

- **The age-based ephemeral auto-reaper now honours the dirty-tree guard**
  ([#4091](https://github.com/bobmatnyc/trusty-tools/issues/4091), review
  round): `reap_aged_ephemeral` reaches the same
  `worktree remove --force` → `remove_dir_all` → `branch -D` sequence as the
  orphan sweep but never passed through the guard, and it fires automatically
  on every daemon GC tick. An aged ephemeral whose workspace holds unsaved work
  is now left in place for an operator. There is deliberately no discard opt-in
  on this path, and the check is unconditional rather than restricted to
  `.worktrees/<leaf>` paths — `decommission` also `remove_dir_all`s the whole
  workspace for `workspace_owned` records, which a path-shape filter would have
  excused entirely.

- **The idle nudge is no longer suppressed indefinitely by a single delegation**
  (#2864): `has_live_children` treats `Queued`/`Running` as live, but nothing
  ever advanced a delegation out of `Queued`, so any session that had delegated
  once looked permanently busy. Delegations now terminalize on `SubagentStop`,
  correlated exactly by `agent_id`, so concurrent subagents close independently
  and finishing one never closes another. `SubagentStop` is not guaranteed to
  arrive, so a background sweep additionally bounds liveness (below) — together
  these cover both the hook-observed and the never-observed cases.
- **A delegation whose `SubagentStop` never arrives can no longer pin a session
  "busy" for the daemon's lifetime** (#2864 review): a dropped hook POST, an
  interrupted subagent, or a dispatch that never learned an `agent_id` left a
  `Running` record with no route out. The 60-second reap loop now sweeps
  delegations: a live record past its budget (6 hours for `Running`, 15 minutes
  for a `Queued` `agent_delegate` declaration that was never dispatched) is
  marked with the new `stale` status. `stale` deliberately means "tracking lost
  this", **not** "the agent finished" — it stops counting as live but is not
  terminal, so a late `SubagentStop` still resolves it to the truth.
- **The delegation map is bounded, without expiring the `stale` recovery
  window** (#2864 review): a terminal delegation is evicted an hour after
  `ended_at`; nothing can resolve a terminal record, so that costs nothing. A
  `stale` one is held for 24 hours from its own start — an **18-hour** window in
  which a late `SubagentStop` still resolves it — and a live one is never
  evicted at any age. The sweep does not stamp `ended_at` on `stale`, so that
  field keeps meaning exactly "reached a terminal status". Past 24 hours a
  `stale` record is dropped and a later stop finds nothing: the recovery window
  is long, not infinite, because the map must stay bounded.
- **A `PostToolUse` no longer marks a running subagent `Completed` on an absent,
  unparseable, or `agentId`-bearing response** (#2864 review): the launch
  handler inferred "not async, therefore finished" from an absent response, so a
  Claude Code response-shape change would have silently reported "no agents in
  flight" ~1 ms after every dispatch. The response is now classified three ways
  — *launched* / *returned* / *unknown* — and only *returned* completes. A
  response carrying an `agentId` counts as *launched*, since that key exists
  precisely so a future `SubagentStop` can quote it; marking such a dispatch
  complete while storing the id to expect a stop for was self-contradictory. The
  recognized key set is defined once and shared by the `tm hook` projection and
  the daemon so the two cannot drift. *Known limitation:* a recognized response
  that carries no `agentId` and is silent about liveness is still read as a
  synchronous return — such a response has no other route to termination, so
  tightening it is blocked on giving `SubagentStop` a recovery path first.
- **Two same-agent dispatches inside the dedup window are no longer merged into
  one record** (#2864 review): dedup matched on the agent name alone, so a
  declaration plus an unrelated later dispatch of the same agent collapsed
  together — undercounting work in flight and displaying the declaration's task
  text for the dispatch that was actually running. The task description is now a
  required discriminator (whitespace/case-insensitive prefix match either way; a
  dispatch with no description declines to merge at all), and the window is 120
  seconds rather than 300.

- **Bare `tm` no longer misreports a managed pane as "not a git project"**
  (closes [#4061](https://github.com/bobmatnyc/trusty-tools/issues/4061)):
  when a managed pane was still settling, the in-place relaunch could observe
  the session as not-yet-resolvable and fall through to the generic
  non-git-project hint — telling the operator their repo wasn't a git project
  when it plainly was. Bare `tm` now retries the in-place relaunch when an
  env-resolved managed session id exists, and if the session still hasn't
  settled it prints an honest "still settling" message instead of the
  misleading non-git hint.

- **A session worktree's branch is no longer left tracking the default branch
  unless a bare `git push` is provably confined to its own branch**
  ([#2867](https://github.com/bobmatnyc/trusty-tools/issues/2867)):
  `configure_session_branch_tracking` used to set
  `branch.<session>.merge = refs/heads/<default>` FIRST and pin
  `push.default = current` afterwards, both best-effort — so any failure of the
  pin (old git, unwritable base config, sandboxed `git config`) left the
  worktree armed with a foreign upstream. The pin is now established first and
  read back from the effective config stack; when it cannot be confirmed the
  worktree is left with no upstream at all (`git pull origin <default>` still
  works) rather than an upstream it does not own.

- **Concurrent workspace-trust seeds no longer clobber each other's
  `~/.claude.json` entries** (closes
  [#4072](https://github.com/bobmatnyc/trusty-tools/issues/4072)):
  `core::home_trust_seed::preseed_home_trust` and
  `core::session_launch::settings::preseed_workspace_trust` both perform a
  load → mutate → store cycle over the same `~/.claude.json`, and the daemon
  runs them concurrently — once per session it provisions, from independent
  tokio tasks. Unsynchronised, the slower writer serialised the snapshot it
  read *before* the faster writer's store, silently deleting that session's
  whole `projects.<workspace>` entry (trust acceptance **and** its
  `enabledMcpjsonServers` approval) with nothing logged; the operator then hit
  the exact "Do you trust the files in this folder?" / "new MCP servers found"
  dialog the seed exists to dismiss. Both seeders now hold one process-wide
  lock (`core::claude_json_guard`) across the whole cycle, and
  `preseed_workspace_trust` writes through
  `trusty_common::claude_config::write_json_atomic` instead of a truncating
  `std::fs::write` — a half-written file was observable by any concurrent
  reader, and every reader in this crate treats malformed JSON as "skip,
  leave it alone". This also fixes the recurring CI flake in
  `tests_manifest_toggle_trust_3934` (red on PRs #4057 and #4067, green in
  isolation), where a non-serial test resolving the process-global `$HOME`
  clobbered a `#[serial]` test's temp `.claude.json`.

- **Activity monitor no longer misreports a provider stream failure as a
  serialization error** (issue #3757): `LlmActivityClassifier::classify`
  matched only `ChatEvent::Delta` and discarded `ChatEvent::Error`, so once the
  shared SSE pump began reporting mid-stream error frames, truncated EOFs, and
  unusable non-SSE bodies, the truncated text still reached `serde_json` and
  surfaced as `ActivityError::Serialization` — blaming our own parser for
  someone else's outage. The error arm now maps to `ActivityError::Llm`.

- **`prune-worktrees` never scanned `.claude/worktrees`, so Claude Code's
  native agent worktrees were never reclaimed** (closes
  [#3971](https://github.com/bobmatnyc/trusty-tools/issues/3971)):
  `find_orphaned_worktrees` walked only the two trusty-mpm-owned shapes
  (`.worktrees/<name>`, `.base/.worktrees/<session-id>`) and never
  `.claude/worktrees/<name>` — the path Claude Code's native
  `isolation: "worktree"` mechanism uses — so those worktrees accumulated
  silently, invisible to `tm session prune-worktrees` and `tm doctor`. Now
  scanned as a third shape via the same `scan_worktree_shape` walk; the
  existing #3649 ownership-sentinel gate already treats a
  Claude-Code-created worktree (which never carries trusty-mpm's
  `.trusty-mpm-worktree` sentinel) as owner-unknown, so newly-discovered
  candidates are surfaced for `tm doctor`/`--dry-run` review, never
  auto-deleted. Follow-up review found the initial fix anchored only at
  `<repo>/.claude/worktrees`, missing two sibling locations confirmed live
  with real entries: `<repo>/.base/.claude/worktrees` (agent worktrees
  created directly inside the bare `.base` clone checkout) and
  `<repo>/.base/.worktrees/<session-id>/.claude/worktrees` (agent worktrees
  nested inside a per-session checkout) — both are now scanned too.

- **A toggle being on was treated as proof of a successful MCP-server pin,
  even when the force-overwrite write itself failed** (closes
  [#3950](https://github.com/bobmatnyc/trusty-tools/issues/3950), fifth
  instance of the name-approval/content-pinning class after
  [#3918](https://github.com/bobmatnyc/trusty-tools/issues/3918)/
  [#3924](https://github.com/bobmatnyc/trusty-tools/issues/3924)/
  [#3926](https://github.com/bobmatnyc/trusty-tools/issues/3926)/
  [#3934](https://github.com/bobmatnyc/trusty-tools/issues/3934)): a disk-full,
  permission, or transient I/O fault during `inject_trusty_memory_mcp`/
  `inject_trusty_search_mcp`/`inject_trusty_mpm_mcp`/`inject_trusty_review_mcp`
  is non-fatal to launch, but the toggle/attempted-injection value was still
  used to approve the name in `enabledMcpjsonServers` regardless of whether
  the write actually succeeded — leaving a spoofed or stale `.mcp.json` entry
  pre-approved with unverified content behind it. `mcp_config::managed_mcp_server_names`/
  `launch_trusted_mcp_names`/`launch_trusted_mcp_names_from` and
  `standalone::trust_seed::preseed_managed_trust` now take each of the four
  builtins' actual per-run pin result as an explicit `_pinned` bool (not a
  toggle); `session_launch::PrepReport` gained
  `trusty_mpm_injected`/`trusty_review_injected`/`trusty_memory_injected`/
  `trusty_search_injected` fields so `standalone::load::load_alias` (the `tm
  load`/`tm run` daemon-managed driver) can read the SAME run's actual
  outcome instead of re-deriving from the manifest toggle alone.
  `runtime::claude_code::prepare_managed_config` (the tmux daemon-spawn
  adapter behind `RuntimeAdapter::spawn`/`spawn_resume`) now pins all four
  builtins itself and derives trust membership from their real `Result`s
  too — critic follow-up review found this call site reachable with ZERO
  same-run evidence at all on the headless `spawn_resume`/`session_resume`
  path (no `prepare_session*` call anywhere in that request's chain), the
  sharpest case across all five instances of this vulnerability class.

- **An untrusted project manifest could disable the `trusty-memory`/
  `trusty-search` force-overwrite injector while the name stayed
  pre-approved, reopening builtin-name-squatting** (closes
  [#3934](https://github.com/bobmatnyc/trusty-tools/issues/3934)):
  `mcp_config::managed_mcp_server_names`/`launch_trusted_mcp_names` trusted
  the two conditionally-injected builtins unconditionally, on the assumption
  their force-overwrite injector always ran. That assumption was controlled
  by the manifest's `[mcp] trusty_memory`/`trusty_search` toggle — untrusted,
  project-scope, cloned-with-the-repo content, unlike `[mcp.custom]` (gated
  by `is_project_trusted` since #2739). Combining a manifest that disabled
  the injector with a spoofed `.mcp.json` entry under the same name left the
  attacker's command pre-approved with no human present to decline it
  (empirically confirmed while attack-testing
  [#3940](https://github.com/bobmatnyc/trusty-tools/pull/3940)). Trust-set
  membership for `trusty-memory`/`trusty-search` is now CONDITIONAL on the
  toggle actually being on for the workspace this run — the tm-launch path
  reads the already-resolved `HarnessPlan` in memory, and the two
  daemon-managed callers re-resolve it via the new
  `mcp_config::resolve_conditional_mcp_toggles` (a pure, side-effect-free
  re-read of the same manifest layering). A disabled injector now correctly
  drops the name from `enabledMcpjsonServers` instead of leaving it approved
  with unverified content behind it; a legitimate operator toggle still
  launches cleanly. Completes the vulnerability-class lineage started by
  [#3918](https://github.com/bobmatnyc/trusty-tools/issues/3918)/[#3924](https://github.com/bobmatnyc/trusty-tools/pull/3924)/[#3926](https://github.com/bobmatnyc/trusty-tools/issues/3926).

- **`tm launch` no longer auto-trusts every MCP server name a repo's
  committed `.mcp.json` declares** (closes [#3926](https://github.com/bobmatnyc/trusty-tools/issues/3926)):
  `preseed_workspace_trust` derived `enabledMcpjsonServers` from the raw key
  set of `<workspace>/.mcp.json` — content a cloned repo controls directly —
  filtered only by a narrow project-scope-custom exclusion (#2739), so any
  MCP server name a repository committed was silently pre-approved and
  connected with the operator's credentials on the first `tm launch` in that
  clone. It now derives from `mcp_config::launch_trusted_mcp_names` (the
  framework builtin four, force-overwritten to their canonical entry every
  run, UNION the operator's own `tm mcp add` registry), mirroring the
  already-fixed daemon-managed-path derivation
  (`managed_mcp_server_names`, [#3924](https://github.com/bobmatnyc/trusty-tools/pull/3924)/[#3918](https://github.com/bobmatnyc/trusty-tools/issues/3918)).
  A name that reaches neither provenance-safe source now correctly falls
  through to Claude Code's own "new MCP servers found" consent dialog instead
  of being silently approved; project-scope `[mcp.custom]` entries remain
  excluded from pre-approval even for a `tm project trust`-ed project,
  unchanged from #2739's existing design. Originates from #1296/#2739.

- **`/tm-session-pause` and `/tm-session-resume` now instruct the PM to load
  the deferred MCP tool before calling it** (closes [#3901](https://github.com/bobmatnyc/trusty-tools/issues/3901)):
  `session_context_pause`/`session_context_catchup` are always registered by
  the daemon, but Claude Code defers loading their schemas until fetched via
  `ToolSearch`, so the PM previously saw the tool missing from its loaded
  list, concluded (wrongly) that it was unavailable, and improvised an
  undocumented "MCP pause tool unavailable" fallback that hand-wrote
  snapshots drifting out of the format `/tm-session-resume`'s parser expects
  — reproduced 6 times in this project's own `.trusty-mpm/sessions/` history
  over 3 days. Both skills now mandate a `ToolSearch(query:
  "select:mcp__trusty-mpm__<tool>")` load step before the call, and replace
  the improvised fallback with an ordered, evidence-based diagnostic
  procedure: attempt the load, attempt the call, report the actual observed
  error text (never an unverified guess like "the server isn't connected" —
  that claim is only valid when checked against whether any
  `mcp__trusty-mpm__*` name appears anywhere in the loaded-or-deferred tool
  list), and only then hand-write a snapshot — which must now
  self-identify as a fallback with the observed failure recorded, so a
  resumed session can tell at a glance it wasn't tool-generated. The same
  deferred-tool-loading gap and fix pattern was also applied to
  `tm-bug-reporting` and `tm-postmortem` (same `mcp__trusty-mpm__*`
  procedural-call shape), with a canonical "Deferred MCP Tool Loading"
  reference added to `tm-tool-usage-guide`.

---
## [1.0.2] - 2026-07-25

### Added

- **`tm compress` durable compression-effectiveness telemetry (closes [#3870](https://github.com/bobmatnyc/trusty-tools/issues/3870), epic [#3866](https://github.com/bobmatnyc/trusty-tools/issues/3866) Slice D):** every rewritten Bash tool call piped through `tm compress` (issue #1956's PreToolUse spike) now appends a durable JSONL record to `~/.trusty-mpm/compression.jsonl` — `surface`, `surface_detail` (tool name), byte-proxy `tokens_before`/`tokens_after`, `pct_reduction`, and `compression_path` (`"rtk_binary"`/`"native_fallback"`) — alongside the existing stderr `tracing::info!` stats line (still emitted, unchanged). Awaited inline rather than spawned, since `tm compress` is a short-lived pipe filter with no background runtime to hand a detached write off to.

- **Guided picker `ls`/`list` command** (closes [#3863](https://github.com/bobmatnyc/trusty-tools/issues/3863)): the bare-`tm` guided session picker's `tm: >` prompt now accepts `ls` or `list` (case-insensitive) to re-print the current session list in place, refreshed from the daemon, rather than only supporting a numbered choice, `d<N>`, `r<N> <new-name>`, or `q`. An unrecognised choice at the prompt no longer quits the picker outright — it now prints a one-line hint of the accepted tokens (`1..N, ls, d<N>, r<N> <name>, q`) and redisplays the same menu.

### Fixed

- **Daemon-managed sessions (`tm session new`, agent delegations) now trust the project's own `trusty-mpm` MCP server** (closes [#3918](https://github.com/bobmatnyc/trusty-tools/issues/3918), sibling of [#3901](https://github.com/bobmatnyc/trusty-tools/issues/3901)): `runtime::claude_code::prepare_managed_config` seeded MCP trust into `<CLAUDE_CONFIG_DIR>/.claude.json` — the file a managed session actually reads — from a hardcoded three-server list (`trusty-memory`/`trusty-review`/`trusty-search`) that structurally excluded `trusty-mpm` itself, while the correct, dynamically-derived list (from the project's real `.mcp.json`) was written to the operator's real `~/.claude.json`, which a managed session never reads once `CLAUDE_CONFIG_DIR` is set. `trusty-mpm` now joins `mcp_config::BUILTIN_MANAGED_MCP_SERVERS` (its launch command is framework-controlled, not sourced from repo content, so it is safe to trust unconditionally like the other three) and is a reserved name no `[mcp.custom]`/`tm mcp add` entry can shadow; `standalone::trust_seed::preseed_managed_trust` continues to derive `enabledMcpjsonServers` from `mcp_config::managed_mcp_server_names` (the framework four ∪ the operator's own `tm mcp add` registry) — an earlier version of this fix instead derived the list from the workspace's own `.mcp.json`, but a code-critic security review caught that this would let a hostile or compromised cloned repo's `.mcp.json` get silently auto-approved and connected in a daemon-managed session with no human present to decline it, so that approach was rejected in favor of sources that cannot be influenced by repo content. The standalone `tm run` driver's own global `.mcp.json` (`standalone::global_config::ensure_mcp_config`) and `tm mcp test` gain the same `trusty-mpm` entry for free via the shared builtin catalog.

  **Second code-critic BLOCK, empirically reproduced exploit — name-squatting on an unconditionally-approved name:** `enabledMcpjsonServers` approval is NAME-based and CONTENT-BLIND — Claude Code still reads whatever `command` sits under an approved name in `<workspace>/.mcp.json` (the project layer). `trusty-memory`/`trusty-search` have dedicated injectors (`inject_trusty_memory_mcp`/`inject_trusty_search_mcp`) that force-overwrite that entry on every `prepare_session` run, but `trusty-mpm` and `trusty-review` had none — so (a) a hostile clone could commit a spoofed `trusty-mpm` entry pointing at an attacker binary and have it execute with the operator's credentials on the very first `tm session new` (the name is pre-approved, nothing overwrites the entry, no human is present), and (b) a benign freshly-cloned project with no committed entry got NOTHING under that name, so #3918's actual symptom (the PM unable to call `mcp__trusty-mpm__*` tools) was only fixed for a project that happens to already commit its own entry, not the general case. `session_launch::settings::inject_trusty_mpm_mcp` and the new `inject_trusty_review_mcp` now force-write `mcp_config::builtin_server_entry`'s canonical command for both names on every `prepare_session` run, mirroring the existing memory/search injectors — closing the exploit and actually fixing #3918's symptom in the same change, since the approved name and the on-disk entry now come from the same pipeline. `trusty-review`'s gap predates this PR chain (it has been in `BUILTIN_MANAGED_MCP_SERVERS` since before #3918) but is the identical exposure, so it is fixed here too rather than left open.

- **`tm sessions list`/`tm project list` now show a session created via `sessions new <url>`** (closes [#3822](https://github.com/bobmatnyc/trusty-tools/issues/3822)): `ProjectRegistry::auto_register_from_sessions` only ever ran ONCE per daemon process — inside `DaemonState::project_registry()`'s `OnceCell::get_or_init`, seeded from whatever session snapshot existed the moment ANY of its 15+ call sites (a status poll, an MCP tool, `resolve_gh_env`, …) first touched the registry, frequently before any session existed. Nothing else called that pass again, so a project implied by a session spawned afterward was registered nowhere — even though the daemon plainly knew about the session (`tm stop`/`tm resume` by explicit name still worked, since those go through the separate session-manager store). Added `ProjectRegistry::register_from_session`, an explicit per-session counterpart called at spawn time from all three managed-session creation paths (`spawn_managed_cloned`, `spawn_managed_inproject`, `spawn_managed_local` in `daemon::managed_routes::lifecycle`), right after the session record is persisted — so registration no longer depends on which caller happened to touch the registry first. Hardening (code-critic review): `project_registry()`'s session-history seed now warms `session_manager()` itself (`self.session_manager().await`) instead of peeking at `self.managed_sessions.get()`, removing an unenforced "something else must touch `managed_sessions` first" startup-ordering invariant that could have silently resurrected this bug for PRE-EXISTING sessions on any future call-order reorder. **Operational note: this fix takes effect on daemon RESTART, not as a hot-reload** — a currently-running (pre-fix) daemon process must be restarted onto the new binary before its `project_registry()`'s one-shot boot seed re-scans existing sessions; newly-spawned sessions become visible immediately once the daemon is running the new binary regardless of restart timing (that half is the `register_from_session` spawn-time call, not the boot seed).

- **`tm sessions new`/`tm resume` no longer 500 on a machine where tmux has never run** (closes [#3823](https://github.com/bobmatnyc/trusty-tools/issues/3823)): session creation's FIRST tmux call was a raw `list-sessions` probe (`SessionManager::resolve_session_name`/`dedupe_session_name`'s name-collision checks — issued well before the #3386/#3722 `create_managed_session` choke point, which already guaranteed server-up but only for its own `set-option -g`/`new-session` sequence), so a fresh socket directory's "No such file or directory" error propagated straight to the caller as `TmuxUnavailable` instead of transparently starting the server. Added `ManagedTmuxDriver::ensure_server_up` (default no-op for test doubles; `RealTmuxDriver` delegates to the new `TmuxDriver::ensure_server_up`, which reuses the existing retried-with-backoff `core::tmux::ensure_server_up` primitive) and call it at the top of `SessionManager::create_with_id`, `create_with_reserved_name`, and `resume` — before any other tmux call in those flows.
---
## [1.0.1] - 2026-07-24

### Fixed

- **Linux release build no longer requires system OpenSSL** (closes [#3538](https://github.com/bobmatnyc/trusty-tools/issues/3538)): the workspace `teloxide` dependency (root `Cargo.toml`, inherited by `trusty-mpm`'s optional `telegram` feature — part of the default `cli` build) defaulted to teloxide's `native-tls` feature, which links `openssl-sys` against a system-installed OpenSSL. macOS builds were unaffected (`native-tls` uses Security.framework there), but the Linux release environment has no system OpenSSL dev headers, breaking the `tm` binary build/release. Switched to `default-features = false, features = ["rustls", "macros", "ctrlc_handler"]` — TLS now goes through `rustls` (no system OpenSSL dependency on any platform) while preserving the two other default features the code actually uses (`macros` for the `BotCommands` derive, `ctrlc_handler` for `Dispatcher::enable_ctrlc_handler()` in `telegram/mod.rs`). Verified: `cargo tree -p trusty-mpm -e normal,build -i openssl-sys` / `-i native-tls` are empty for the `trusty-mpm` release-binary dependency graph (both `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` targets); git2's own (unrelated, pre-existing) `openssl-sys` usage is `vendored-openssl` and self-compiles rather than requiring a system install.

---
## [1.0.0] - 2026-07-24

**First stable release.** trusty-mpm (the `tm` harness/CLI/daemon) has been in
daily driver use for several weeks across the maintainer's own projects; this
release marks it semver-committed / stable rather than signaling any single
breaking change. Sibling crates (trusty-common, trusty-search,
trusty-memory, etc.) are unaffected and keep their own independent 0.x
version lines.

### Added

- **Per-project "launch on main" worktree opt-out** (closes [#3455](https://github.com/bobmatnyc/trusty-tools/issues/3455)): a registered project's `worktree` field (`Project::worktree: Option<bool>`, default unset = `true`) can now disable per-session git worktree provisioning for that project — set it via `tm projects config <name> set worktree false` (round-trips through the existing `PATCH /api/v1/projects/{name}` configurator, `tm projects config <name>` shows the resolved state) or via `config.yaml`'s `projects[].worktree` (mirrored into the registry at seed time). When disabled, `daemon::managed_routes::lifecycle::spawn_managed_routed` skips the base-clone-plus-worktree path entirely — no `.base`/`.worktrees` directory, no `session/<name>` branch — and spawns the managed session directly in the project's resolved local checkout via the new `spawn_managed_on_main`, which is otherwise a normal managed session (agents/skills deployed, hooks written, tracked/reconnectable, front-gated) with `workspace_owned = false` so decommission never touches the operator's own directory. Running two sessions against the SAME main checkout is not isolated the way two worktrees are; a `warn!` (never a refusal) fires when a second `Active` session targets the identical path, which only a `--force-new` launch can produce (the normal reconnect pre-flight already prevents it otherwise).

---
## [0.21.0] — 2026-07-23

### Added

- **Cross-workstream coordination via trusty-memory claim drawers** (DOC-53, [docs/specs/DOC-53-workstream-claim-drawer-convention.md](../../docs/specs/DOC-53-workstream-claim-drawer-convention.md), motivated by epic #3524 slices 5+6 independently duplicating `/health` work): `PM_INSTRUCTIONS.md` gains a concise "Cross-Workstream Coordination" section — before dispatching multi-agent work on an area, recall `memory_list(tag: "ws-claim")` and verify any hit against live git/GitHub state (a claim whose branch/PR no longer exists is void); write a `WS-CLAIM <workstream>: <area>` drawer when dispatching; supersede it on land or abandon. Memory remains awareness-only — git/GitHub branch/PR/label state stays the authoritative claim and the event bus (#3168 BUS-7) remains the real-time channel; this does not add any new memory locking or messaging primitive.

- **`ws/<session-name>` workstream-activity labels — ensure at launch + PM-brief convention** (closes [#3726](https://github.com/bobmatnyc/trusty-tools/issues/3726)): workstream activity is now tracked on GitHub via a `ws/<session-name>` label — never a milestone, which stays reserved for epics/releases (a repo allows only one milestone per issue/PR, a slot epics/releases already need; labels are cheap, multi-valued, and filterable instead). A new `core::session_launch::workstream_label` module derives the session's `owner/repo` from its `repo_url` (falling back to the workspace's own `git remote get-url origin`), and idempotently ensures `ws/<session-name>` exists via `gh label create --force` with a stable, name-derived color (FNV-1a hash → HSL hue). The three `daemon::managed_routes::lifecycle::spawn_managed_*` fresh-launch branches fire it detached (`tokio::task::spawn` + `spawn_blocking`, bounded by a 5s timeout) so it never adds latency to — or can ever fail — a session launch; every non-GitHub-remote case (no origin, non-GitHub host, `gh` missing/unauthenticated/offline) is a logged, non-fatal skip. The bundled `PM_INSTRUCTIONS.md` (plus `tm-pr-workflow`/`tm-bug-reporting`/`tm-ticketing` skills) now extend the existing `--label trusty-mpm` issue/PR convention to also apply `--label ws/<session-name>` (the PM's own tmux session name via `tmux display-message -p '#{session_name}'`). Rename-time label ensure is deferred to a follow-up (kept out of `session_manager::rename` to stay clear of a concurrent in-flight PR touching that file). Label names are capped at GitHub's 50-char limit — a session name that would overflow `ws/<name>` is truncated with an 8-hex FNV-1a suffix for uniqueness (an operator `tm sessions rename` can produce names up to 64 chars, well past the 47-char budget).

- **Session-owned worktrees — ownership registry + owner-gated reclamation** (closes [#3649](https://github.com/bobmatnyc/trusty-tools/issues/3649), Option B, see [ADR-0020](../../docs/adr/0020-session-owned-worktrees.md)): the per-worktree `.trusty-mpm-worktree` sentinel now carries a JSON payload (`{"owner_session_id", "created_at"}`) instead of a zero-byte marker, and `SessionRecord` gains a `worktree_owner` registry field set immediately after provisioning. `SessionManager::decommission` accepts an optional `caller` identity and refuses to reclaim a target whose worktree has a KNOWN owner that disagrees with the caller (unless that owner is provably ownerless — no resolvable record, or a terminal-state one); operator/daemon-internal callers (`caller: None`) are never gated. The orphan-GC sweep (`find_orphaned_worktrees`) now ALSO walks the clone-based `.base/.worktrees/<session-id>` shape (previously invisible to `tm doctor` and `--dry-run`), and never auto-deletes an owner-unknown worktree — legacy (pre-#3649) worktrees stay owner-unknown forever (zero migration) and keep surfacing via the existing `prune --dry-run`/`tm doctor` flows for explicit human action. The out-of-scope harness `.claude/worktrees/` store is documented as a reserved-compatible shape only. A pre-merge review found that the sentinel is written before the owning session's record is persisted (real clone/`prepare_session` I/O runs in between), so an orphan-GC sweep landing in that window could have mistaken a brand-new, mid-creation worktree for an ownerless one; `worktree_ownership::OWNERLESS_GRACE` (10 minutes) plus `resolve_ownerless_with_grace` close that window — an absent-owner sentinel is only reclaimed once it is older than the grace period.

### Fixed

- **`tm ls`/bare-`tm` picker rows carried a leading `tm:   ` log prefix instead of leading with the `[N]` index** (closes [#3741](https://github.com/bobmatnyc/trusty-tools/issues/3741)): every picker menu row — `format_session_row`'s normal/deleted/unresumable rows plus the trailing `[N] launch new session`, `[d<N>] delete session N`, `[r<N> <new-name>] rename session N`, and `[q] quit` rows in `run_tty_picker` — printed `tm:   [N] name (state)` (e.g. `tm:   [11] resume tm-tagents (attached)` pre-[#3730](https://github.com/bobmatnyc/trusty-tools/pull/3730)), pushing the operator-scanned index off the left margin. Rows now render as `[N] name (state)` with no prefix; the `tm:` prefix is preserved everywhere else (warnings, prompts, and confirmation lines are genuine log output, not menu rows). A code-critic pass on the PR caught a second, uncoordinated copy of the same shape: `guided::print_project_context` (printed by `tty_gate` immediately BEFORE `run_tty_picker` on every interactive bare-`tm` invocation) still emitted `tm:   [N] name  state=… last=…`, so bare `tm` briefly showed the session list twice in two different row formats on the same screen. That row is now produced by the same prefix-free convention via a new pure `format_project_context_row` helper (kept on its own plain 1-based numbering, deliberately not unified with the picker's stable `shown_slot` numbering — tracked as a possible follow-up, not in scope here).

- **Stop/reap could silently recreate a vanished session workspace root as a bare, non-git shell** ([#3715](https://github.com/bobmatnyc/trusty-tools/issues/3715), hardening only — the original deleter is still unpinned): `session_manager::snapshot::write_scrollback`'s `tokio::fs::create_dir_all(parent)` recreates every missing ancestor directory, so a stop/reap (`SessionManager::stop`, `mark_runtime_exited_stopped`/`runtime_reap`) on a session whose `workspace_path` had vanished from disk would silently reconstitute a bare `.trusty-mpm/`-only shell in its place — no `.git`, no source tree — logging only at `debug!`. `capture_into` now checks the workspace ROOT's existence before ever touching the filesystem and refuses the scrollback write with a `warn!` naming the session and path when the root is missing (losing a scrollback snapshot is acceptable; fabricating a workspace shell under a live session record is not); `write_scrollback` independently re-checks the root immediately before `create_dir_all` (a code-critic TOCTOU finding — `driver.capture()` runs a real, potentially-blocking `tmux capture-pane` subprocess between the two checks, a genuine window for the root to vanish; a residual instant-scale race between the re-check and `create_dir_all` remains and is accepted, documented in `write_scrollback`'s doc comment). The normal first-write case (root present, `.trusty-mpm/` subdir absent) is unaffected and now logs at `info!` instead of `debug!` when it actually creates the subdir. Separately, the `#1845` F3 canonicalize-fallback WARN in `prune_orphaned_worktrees` (the periodic ~60s orphan-GC loop, the `prune_worktrees` MCP tool, and the `prune-worktrees` HTTP route — all three real, deletion-capable sweeps share one counter) now tracks a per-path consecutive-failure streak and escalates to `error!` once a path has failed 10 consecutive real-sweep observations in a row (not a fixed wall-clock duration, since a manual sweep can interleave with the periodic loop), evicting a path's streak once it leaves the active-session set so the counter cannot grow unbounded — the original incident's F3 WARN fired unnoticed for ~8h before this streak alarm existed.

- **Relaunch/rename resolvers picked the wrong record on a duplicated session name, and `tm sessions rename` could physically rename an unrelated LIVE tmux session out from under an attached operator** (closes [#3714](https://github.com/bobmatnyc/trusty-tools/issues/3714), the missing disambiguation layer on top of [#3698](https://github.com/bobmatnyc/trusty-tools/pull/3698)'s new-collision prevention): when two managed records shared a display name (the already-duplicated condition #3692/#3698 evidence, or any other duplication vector), the bare-`tm`-inside-tmux nested-session guard's tie-break (`nested_managed_match`) picked the most-recently-CREATED candidate with no regard for which one's pane was actually alive — a stale duplicate created after the real session reliably won, producing the reported dead-pane refusal. It now prefers, in order: (1) the invoking process's own confirmed tmux pane identity when running inside a managed session, (2) a candidate whose pane is verifiably alive over one that is not, (3) — failing that — any non-decommissioned candidate over a decommissioned tombstone (matching the pre-fix behavior exactly, so a genuinely `stopped` record can never lose to a newer tombstone), (4) the most-recently-created as the final tie-break — and REFUSES with a full-session-id disambiguation listing (`pick_least_ambiguous`) rather than silently picking a row when a tie survives every tier. Separately, and at higher severity, `SessionManager::rename`'s "physically rename the tmux session to match" step resolved the tmux session to retarget by a bare NAME-STRING membership check (`live_names.contains(old_name)`) — under the same duplicate-name condition, renaming a stale record whose own pane was gone matched and renamed a DIFFERENT, live, attached operator's session that merely shared the old name (reproduced live: `tm sessions rename` on a stale `tm-tagents` duplicate retitled the real, attached `tm-tagents` session out from under its owner). `rename` now resolves tmux liveness via the TARGET record's own recorded `pane_id` (pane-scoped, via `ManagedTmuxDriver::pane_exists`, fail-closed as befits a mutation guard) and renames only the DB record — with a clear `warn!` notice — when that identity can't be confirmed, never touching an unrelated live session. Both fixes share their root cause with a third: `reconcile_live_state` (the `tm ls`/`session info` display-state reconciler) used the same name-string-membership check, which is why a single record could simultaneously report `state: "active"` and `persisted_state: "stopped"` — different code paths reading `state` vs `persisted_state` disagreed about the identical row, producing the observed flip-flopping banners across two consecutive identical `tm` runs. It now reconciles `state` pane-scoped too, via a new tri-state `ManagedTmuxDriver::pane_exists_checked` that distinguishes "confirmed absent" (closes the contradiction) from "the `list-panes` query itself failed" (falls back to the name-only signal instead of asserting absence, so a transient tmux hiccup can no longer flip a genuinely live record's DISPLAYED state to `"stopped"` — the display path fails OPEN toward the weaker signal, deliberately the opposite of the mutation-guard's fail-CLOSED `pane_exists`).

- **Resume-after-tmux-server-death applied `history-limit` before the socket existed, so the recreated pane silently got tmux's factory 2000-line scrollback** (closes [#3386](https://github.com/bobmatnyc/trusty-tools/issues/3386); trusty-agents' independent tmux implementation has the identical race, tracked separately as [#3729](https://github.com/bobmatnyc/trusty-tools/issues/3729)): unlike `new-session`, tmux's `set-option -g` has no auto-start-server behavior — it fails "no server running" if issued before any server-starting command has run. The resume/recreate path (`SessionManager::resume` → `create_managed_session`, the single session-creation choke point) issued the `history-limit`/`mouse` `SetGlobalOption` calls first and logged their failures as non-fatal, so a session resumed right after its tmux server died got its pane created by the subsequent `new-session` call against a freshly-spawned server that never received the configured option — the pane froze at tmux's default of 2000 lines and silently evicted scrollback for the rest of the session. `create_managed_session` now runs `apply_and_verify_scrollback_options`: an explicit `start-server` (new `TmuxCommand::StartServer`, idempotent) first, then the scrollback options, then a verification probe (new `TmuxCommand::ShowGlobalOption`, `show-options -g -v`) — retrying the WHOLE cycle (not just server-up) up to 3 times, since a successful `start-server` alone does not prove the following `set-option -g` landed. A code-critic review of the first version flagged that exhausting every retry only logged a `warn!` and proceeded — the same silent-degrade #3386 was filed against; `create_managed_session` now returns a `ManagedSessionOutcome { output, options_verified }` and every call site surfaces a degraded outcome to the operator (daemon/TUI paths log `error!` naming the session via a new shared `warn_if_options_unverified` helper; the `tm launch`/`tm connect`/`tm session start` CLI paths print a one-line notice) rather than only being grep-able in the log.

- **`tm ls`/bare-`tm` picker rows printed a confusing restart/resume verb, had no color-coded state, and could print a duplicate row number** (closes [#3723](https://github.com/bobmatnyc/trusty-tools/issues/3723)): rows are now verbless (`[N] <name> (<state>)` instead of `[N] restart|resume <name> (state)`) — the verb was an implementation detail of *how* the session gets reached, decided at selection time, not a property of its current state. The state word is now color-coded (TTY-aware, honoring `NO_COLOR`): active=green, attached=bold cyan, stopped=dim, errored/dead=red, provisioning=yellow. Sync audit: the picker already consumes the daemon's server-reconciled `state` (via `GET /api/v1/sessions/managed` → `numbered_summaries` → `reconcile_against_tmux`), not a raw persisted record — the stale-state symptom reported (`[11] restart tm-tagents (stopped)` for a session live earlier the same day) traces to `reconcile_live_state`'s pre-[#3714](https://github.com/bobmatnyc/trusty-tools/issues/3714) bare tmux-session-NAME-string matching (no per-record pane-identity check), fixed upstream in [#3719](https://github.com/bobmatnyc/trusty-tools/pull/3719) — the picker inherits that fix automatically since it already routes through the shared reconciliation pipeline. Separately, the duplicate-`[N]` defect (`[31]` printed for both a real session AND "launch new session") was a genuine picker-side bug: the "launch new" number was computed as `sessions.last().slot + 1`, safe only while the list stayed in the daemon's ascending-slot order — [#3483](https://github.com/bobmatnyc/trusty-tools/issues/3483)'s `Recent`/`Alpha` sort reorders that same slice before either call site sees it, so the highest-slot session is no longer necessarily last, and `.last().slot + 1` could collide with an existing session's own slot. `next_launch_slot` now takes the true maximum slot across the whole (possibly reordered) list.

- **Session-rename facility surfaced in the picker** (closes [#3724](https://github.com/bobmatnyc/trusty-tools/issues/3724)): the picker now accepts an `r<N> <new-name>` / `r <N> <new-name>` input mode, routed through the existing `commands::rename::do_rename_request` — the same PATCH `/api/v1/sessions/managed/{id}` request `tm sessions rename` issues, including its [#3692](https://github.com/bobmatnyc/trusty-tools/issues/3692) auto-suffix-on-collision behavior and, now that [#3719](https://github.com/bobmatnyc/trusty-tools/pull/3719) is on `main`, [#3714](https://github.com/bobmatnyc/trusty-tools/issues/3714)'s pane-scoped rename-target resolution (`SessionManager::rename` resolves the live tmux session via the target record's own `pane_id` rather than a bare name match) — no tmux-rename logic is reimplemented in the picker.

- **Session names were not enforced unique — two distinct active sessions could share a name** (closes [#3692](https://github.com/bobmatnyc/trusty-tools/issues/3692), distinct from [#3627](https://github.com/bobmatnyc/trusty-tools/issues/3627) which fixed duplicate records for ONE worktree): a live daemon was found with two genuinely different `Active` sessions both named `tm-tagents`, making name-based resume ambiguous and the picker show duplicate rows. `SessionManager::rename` used to hard-reject a name already held by another non-terminal record or live tmux session (`NameCollision`, HTTP 409); `SessionManager::adopt_existing`'s collision guard checked ALL store records regardless of state, so even a `Decommissioned`/`Deleted` tombstone's name wrongly blocked reuse. Per owner decision, every name-allocation site now auto-suffixes on collision instead of ever rejecting, via a new shared `trusty_common::session_naming::dedupe_by_ordinal` primitive (smallest free `-N` ordinal, incrementing an existing trailing ordinal in place rather than double-suffixing — mirrors `trusty-agents`' own `TmManager::unique_tmux_name` convention). `rename` now always succeeds, returning the auto-suffixed name when one was needed. `adopt_existing` now only treats NON-terminal records as taken and, when a name was RECYCLED (a stale non-terminal record's pane has genuinely died and a different live pane now answers to the same name, detected via `pane_id`), auto-suffixes and physically renames the live tmux session to match — while still refusing a genuine duplicate adopt of the identical already-tracked pane. Uniqueness scope is daemon-wide (the whole managed-session set), matching how the picker/resolve-by-name already operate. The existing live `tm-tagents` sessions are left untouched — this is a forward-looking allocation-time fix only.

- **`tm` picker rendered every session as `[0]` and selection resolved to the wrong row against a stale daemon** (closes [#3678](https://github.com/bobmatnyc/trusty-tools/issues/3678)): a daemon process that predates the #3034/#3044 stable-slot-numbering feature omits the `slot`/`deleted` fields from `GET /sessions/managed` entirely, and `ManagedSessionSummary::slot`'s `#[serde(default)]` (added for exactly this forward/backward-compat reason) then silently decoded EVERY row's slot to the shared `0` sentinel instead of erroring — the normal shape of the window between a `tm` CLI upgrade and the operator's next `tm restart`, since installing a new binary does not restart the already-running daemon. The interactive picker (bare `tm` and `tm ls`) then rendered `[0]` on every row, and typed input always resolved to the FIRST session regardless of the number typed (`Vec::position` returns the first match when every `slot` is equal) — display AND selection were both broken. `session_picker.rs` now detects this shape (`slots_are_stale`: every row in a non-empty menu reporting `slot == 0`, impossible for a healthy daemon since slots are unique and 1-based) and falls back to a positional 1-based numbering for that listing, printing an explicit warning pointing at `tm restart`; `find_slot`/`shown_slot` keep the printed menu and accepted input in agreement in both the healthy and stale-fallback cases.

- **Stale-base-checkout error told the reader to `rm -rf` the project's SHARED base directory** ([#3605](https://github.com/bobmatnyc/trusty-tools/issues/3605)): when `<project>/.base` exists but is not a valid bare checkout, `stale_base_dir_error` surfaced a literal, copy-pasteable `rm -rf <path>` as its suggested recovery. That message is consumed almost exclusively by autonomous agents, which execute a suggested command verbatim — and `.base` is the shared git base for every worktree of the project, so following the hint orphans them all (the 2026-07-21 incident: ~70 of 77 worktrees orphaned machine-wide, each then emitting phantom git-discovery test failures). The hint is now a non-destructive QUARANTINE — `mv <path> <path>.stale-<UTC timestamp>` to a timestamped sibling, matching the crate's existing corrupt-state convention (`core::mcp_config`, `core::standalone::trust_seed` both rename rather than delete) — and the message states the shared-ownership blast radius explicitly. A `stale_base_dir_error_suggests_quarantine_not_deletion` regression test asserts the message names no recursive-delete form while still offering a working recovery. Mitigation only; the missing base-directory ownership model remains open as [#3649](https://github.com/bobmatnyc/trusty-tools/issues/3649).

- **Duplicate/stale/crossed managed-session records for one worktree** (closes [#3396](https://github.com/bobmatnyc/trusty-tools/issues/3396)): `reconcile_on_boot`'s external-adopt loop minted a brand-new `SessionRecord` for any live tmux session unknown to the store BY NAME ALONE, never checking whether its resolved cwd already belonged to an existing, non-`Decommissioned` record — so a renamed/replaced tmux session fronting an already-managed worktree got a second, uncorrelated identity (the same defect class #3599 fixed for native-process discovery). The stale sibling's drifted `tmux_name` then made `session_send`/`session_proxy_message` falsely refuse delivery (`PaneGone`) even though the pane was alive under its new name. The external-adopt loop now skips adoption (logging the crossed mapping) when a live session's resolved workspace already has a tracked owner, and a new `plan_workspace_duplicates` pass in `dedup_stale_duplicates` reconciles duplicates already on disk from before this fix — grouping strictly by canonicalized `workspace_path` (independent of `plan_dedup`'s per-repository `source_id`/live-group rules, which correctly never touch this shape): an SM-owned record is never a loser, a live sibling always survives over a dead one, and two simultaneously-live records at one path are left for manual review rather than guessed at.

- **Session-launch no longer registers OS-temp workspace paths with the
  production trusty-search daemon (issue #2914).** `register_project_index`
  now passes `allow_sensitive_path: false` to the shared
  `trusty_common::search_index::ensure_project_indexed` helper — a real
  session workspace is always the user's checked-out repo or a
  `.worktrees/<uuid>` leaf inside it, never a legitimate OS-temp path, so this
  caller never needed the daemon's `SENSITIVE_PATH_PREFIXES` bypass. Closes
  the leak vector that let `tempfile`-backed session-launch test fixtures
  (matching the incident's `*-selfheal-ws`/`*-stale-heal-ws` naming) register
  against whatever trusty-search daemon happened to be discoverable on the
  developer/CI machine.

---
## [0.20.0] — 2026-07-21

### Added

- **`core::deploy_validate`: strict-YAML agent frontmatter check** ([#3556](https://github.com/bobmatnyc/trusty-tools/issues/3556)): `validate_workspace` now catches a deployed `.claude/agents/<name>.md` whose frontmatter fails a strict YAML parse (new `DeploymentGap::AgentFrontmatterInvalid`) — e.g. a copy deployed before the `trusty-agents-common` quote-on-emit composer fix, still carrying an unquoted `description:` with a colon. Previously the probe only checked file presence, so a deployed-but-unparseable agent (11 engineer agents in production) silently failed to load at `tagent`/Claude Code runtime instead of surfacing here at deploy-validation time. Surfaces through `tm doctor`/`tm validate` and the spawn/resume auto-repair gate exactly like every other gap.

- **`session_context_catchup` + `session_context_pause` MCP tools** ([#3543](https://github.com/bobmatnyc/trusty-tools/issues/3543)): the PM's `/tm-session-pause` and `/tm-session-resume` skills now call two new internal MCP tools instead of shelling out to `git log`/`git status` and hand-written snapshot files. `session_context_catchup` returns a structured (JSON) resume digest — paused sessions, recent commits, recent memory-palace activity, `resolved_snapshot`, and `watermark_advanced` (always `false`, a manual peek that never advances the incremental-catchup watermark). `session_context_pause` writes the `session-YYYYMMDD-HHMMSS.md` snapshot in the exact section format the existing reader parses, appends the pause entry to `sessions-log.jsonl`, and by default prunes orphaned managed-session git worktrees in-process (the same engine `tm session prune-worktrees` uses). The `tm session catchup` CLI command is unchanged — the tools are additive.

### Fixed

- **`tm sessions rename` and the `pm_guard` persona gate trusted a bare `TM_MANAGED_SESSION_ID` with no pane-identity cross-check** (closes [#3600](https://github.com/bobmatnyc/trusty-tools/issues/3600)): `TM_MANAGED_SESSION_ID` is published tmux-SESSION-scoped (`tmux set-environment -t <session>`), so a sibling pane opened after a runtime-exit heal inherits an identical value and would be mistaken for that session. `guided.rs`'s nested-session guard already closed this hijack for its own call path by comparing tmux's own stable `pane_id` against the resolved record's captured `pane_id`, but two other consumers read the bare env var unguarded: `tm sessions rename <new-name>` (in-session form — could rename the WRONG session, possibly another user's live one) and the opt-in `TRUSTY_MPM_PM_DENY_BY_DEFAULT` persona gate (could gate a delegated edit on a stranger session's state). Both now share one extracted primitive (`commands::pane_identity::{pane_identity_confirmed, EnvSessionIdentity}`, with `guided.rs` re-exporting it rather than keeping its own copy): `rename` refuses with an explicit error on a pane mismatch or an unresolvable pane id (never silently proceeds); the persona gate discards an unconfirmed session's resolved state entirely, falling back to its existing `Ambiguous` (allow) default rather than treating it as evidence either way.

- **`ensure_gh_account_for_project` (#2081) had zero production call sites** (closes [#3312](https://github.com/bobmatnyc/trusty-tools/issues/3312)): the per-project `gh`-account enforcement added in #2081 existed and was tested, but nothing on a real `gh`/git code path ever invoked it, so a `config_dir` whose isolated store had the wrong account active was never caught. It is now wired at the two real production choke points — `tm ticket`/`tm issue`/`tm watch` (via `gh_identity::resolve_project_aware`) and the daemon's managed-spawn/local-redirect provisioning paths (via the new `git_identity::resolve_for_config_enforced`) — and runs only when a project pairs `github.config_dir` with an explicit `github.account` (a project that only sets one of the two sees no behaviour change). A mismatch is corrected via a `GH_CONFIG_DIR`-scoped `gh auth switch` and re-verified; a switch that cannot succeed hard-fails the operation rather than silently proceeding under the wrong identity.
- **Two spawn sites still bypassed the macOS TCC disclaim wrapper** (closes [#3267](https://github.com/bobmatnyc/trusty-tools/issues/3267), #2997 part 6): `provisioner::clone_progress::clone_with_progress` (the daemon's `git clone --progress` streamer) and `formatters::info_box::probes::run_git_log` (the `tm` welcome panel's `git log` probe) both spawned via a raw `Command::spawn()`, so macOS attributed their file access to the signed `trusty-mpm` binary instead of the child — the same mis-attribution shape fixed elsewhere by #2721/#2957/#3037/#3126. Two new `spawn_disclaim` primitives, `disclaimed_stderr_piped_spawn` and `disclaimed_stdout_piped_spawn`, cover the two single-stream synchronous spawn shapes these call sites need (the existing four primitives only covered capture-to-completion, inherited-stdio, detached, and async three-pipe shapes); both call sites now disclaim TCC responsibility on macOS with no change to their observable behavior.

## [0.19.29] — 2026-07-21

Binary-release re-cut: 0.19.28 was published to crates.io but never got a
GH binary release (the release-workflow's Linux-leg failure incorrectly
skipped the release job entirely — fixed in #3541). This version carries
that CI fix, the zombie-session-restart fix merged since 0.19.28, and the
stable `tm ls` session-numbering fix (#3044/#3034) merged since.

### Fixed

- **`tm ls` session numbers shifted when a session was deleted** (closes [#3034](https://github.com/bobmatnyc/trusty-tools/issues/3034), [#3044](https://github.com/bobmatnyc/trusty-tools/pull/3044)): session numbers were previously recomputed as a plain array index on every fetch, so deleting a session shifted every later row's number — an operator (or PM agent) holding a number from an earlier listing could act on the WRONG session. The daemon now assigns each session a stable slot number the first time it is listed, via a new in-memory-only `SlotRegistry` on `SessionManager` that lives for the daemon process's lifetime (never persisted, so numbering resets cleanly on restart); a deleted session's slot is never reused and renders as a `-- deleted --` tombstone row in both the static `tm session ls` table and the interactive `tm ls`/bare-`tm` picker, which now resolve every typed number (including `d<N>` delete) against the daemon-assigned `slot` field instead of a recomputed position, and report a clear "session N was deleted" error rather than silently falling through to a neighboring row. Tombstones are intentionally shown in the default listing at their original slot — that IS the feature's point — but `tm health`'s `managed_total`/`managed_pending_decisions` fleet counts now exclude them (they are not sessions an operator can act on), and `ManagedSessionView` (the Telegram/Slack/coordinator-TUI view type) now carries the same `slot`/`deleted` fields as the wire DTO so non-CLI consumers can render or skip a tombstone row explicitly instead of showing an indistinguishable blank entry. Reconciled against the #3483 inline sort/filter grammar so the slot each row displays stays stable regardless of the current `recent`/`alpha`/filter view.

- **Resuming a zombie (stale-Active) session from the interactive picker 409'd instead of reconciling** ([#3534](https://github.com/bobmatnyc/trusty-tools/issues/3534)): restarting a session whose tracked state was `Active` but whose underlying tmux/process was already gone returned a 409 in the picker's restart path, instead of the same zombie-reconcile-then-relaunch behavior already applied elsewhere (guided resume, in-place relaunch). The picker's restart path now reconciles a zombie `Active` session before attempting to relaunch it.
- **`.github/workflows/release.yml`'s `release` job silently skipped when a Linux build leg failed** ([#3540](https://github.com/bobmatnyc/trusty-tools/issues/3540)): the job's `if:` condition referenced `needs.build.result` but never called `always()`/`failure()`, so GitHub Actions' default "skip if any dependency job failed" behavior still applied — even though the condition's own comment said it intended to tolerate this. In practice this meant a real (non-AL2023) Linux cross-compile failure (e.g. `git2`'s OpenSSL linking under `cargo zigbuild`) silently blocked the GitHub Release from ever being created, even when the macOS artifact built successfully. This is why the previous `trusty-mpm-v0.19.28` tag produced no binary release at all. Fixed in #3541, carried here to actually exercise the corrected workflow.

## [0.19.28] — 2026-07-20

### Added

- **`tm ls` / `tm sessions ls` gain an inline sort + filter grammar** ([#3483](https://github.com/bobmatnyc/trusty-tools/issues/3483)): `tm ls [recent|alpha] [filter-word...]` — a bare `recent` or `alpha` first word selects the order (`recent`, the default, is most-recently-active first via `last_activity_at` falling back to `created_at`; `alpha` is case-insensitive by session name), and every remaining word (or all words, if the first isn't a sort keyword) becomes a case-insensitive substring filter matched against the id, name, project (`source_id`), state, and task columns. Expressed as positional words rather than `--sort`/`--filter` flags per the repo owner's design call; applies to both the static table and the interactive picker's ordering, with `--json` staying a raw, unsorted/unfiltered passthrough (matching `--all`'s existing "no effect on `--json`" precedent). Backward compatible: bare `tm ls` is unchanged.
- **Native-first MCP connector routing (soft preference)** ([#3480](https://github.com/bobmatnyc/trusty-tools/issues/3480)): the PM's non-overridable `BASE_PM.md` floor and the `BASE-AGENT.md` composed into every deployed agent now state a routing preference for Google Workspace and Slack operations — prefer this workspace's native `mcp__gworkspace-mcp__*` / `mcp__slack-mcp__*` servers over claude.ai's hosted `mcp__claude_ai_Gmail__*` / `mcp__claude_ai_Google_Calendar__*` / `mcp__claude_ai_Google_Drive__*` / `mcp__claude_ai_Slack__*` connectors when both can do the job (ADR-0014). This is a soft preference, not a block — claude.ai's connectors remain available as fallback. `crates/trusty-code`'s embedded `BASE-AGENT.md` copy is updated in lockstep to keep the `check_agent_assets.sh` byte-parity gate green.
- per-session asset staleness detection + `tm sessions sync-assets <id>|--all`: managed-session agent/skill deployment was one-shot at launch (#2002), so a long-lived session never re-synced when the bundled/catalog source changed underneath it. `tm sessions ls` now flags a drifted session with a `[stale-assets]` STATE-column marker (reusing the existing `detect_for_framework` manifest-checksum comparison, re-targeted at each session's own workspace via `FrameworkPaths::for_managed_workspace`), and the new `sync-assets` verb re-runs the SAME roster+output-style deployers `prepare_session` uses at launch against a LIVE session's worktree — overwriting managed files, never a user-modified one — without touching CLAUDE.md, hooks, or MCP config. Exposed as `POST /api/v1/sessions/managed/{id}/sync-assets` (per-session) and `POST /api/v1/sessions/managed/sync-assets` (fleet-wide, `--all`) daemon routes, both gated to `active`/`stopped`/`errored` sessions only — a `decommissioned` tombstone clears `workspace_path` but not `cwd`, so an ungated sync would have silently recreated an already-deleted workspace directory (caught in review). `checked_summaries` now shares ONE `CatalogHashes::compute` catalog-side hash pass across every session on a given `tm sessions ls` call (grouped by each session's resolved `(agent_source, skill_source)` pair) instead of recomposing the ~40+ catalog agents once per session (closes #2444)
- daemon startup version banner (`v<CARGO_PKG_VERSION> pid <pid>`) plus a `tm doctor` stale-daemon check (`daemon_version`) that compares the freshly-installed `tm` binary's version against the running daemon's self-reported `/health` `version` field, warning when the daemon predates the installed binary and needs a restart (closes #2332)
- pm_guard — 3-per-turn file-change budget, read-only-redirection fix, content-aware routing hints ([#2928](https://github.com/bobmatnyc/trusty-tools/pull/2928)) ([`95b69c8`](https://github.com/bobmatnyc/trusty-tools/commit/95b69c80e08e396de8da7fe606d329e8360835a3))
- rust-build-performance bundled skill — dev-loop + dependency-graph build hygiene ([#2929](https://github.com/bobmatnyc/trusty-tools/pull/2929)) ([`c278874`](https://github.com/bobmatnyc/trusty-tools/commit/c278874db20e017323de27129f59647682de643f))

### Fixed

- **Interactive session picker offered "restart (stopped)" for a zombie session, then the restart 409'd** ([#3531](https://github.com/bobmatnyc/trusty-tools/issues/3531)): a session whose tmux window is gone but whose daemon record is still `active`/`provisioning` (a "zombie", e.g. after a reboot) is display-reconciled by the list/get endpoints (#3302) to show `state: "stopped"` for the operator — but that same reconciled value was also fed into the CLI's #2001 zombie-vs-plain-restart classification (`guided_resume::plan_resume`), making a zombie indistinguishable from a genuinely stopped session. The picker's "restart (stopped)" selection then took the plain restart path, and the daemon's `/resume` — which validates the REAL persisted state — rejected it with `cannot resume a session in state 'active'; only Stopped or Errored sessions can be resumed`. `ManagedSessionSummary`/`SessionSummary` gain a new `persisted_state` field carrying the raw, un-reconciled state (set once at conversion time, never touched by the later display reconciliation); the CLI's resume/restart classification (shared by the interactive picker, the bare-`tm` guided default, and `tm session(s) resume <id>`) now keys off `persisted_state` instead of the display value, so the #2001 auto-reconcile (`/runtime-stop` then `/resume`) fires correctly for this case instead of dead-ending. No manual workaround is needed after this fix — simply re-select the session; if still on an older build, run `tm session stop <id>` (or `tm sessions stop <id>`) to force the stale record to `Stopped`, then resume/restart normally.
- **Harness-scaffolding paths could be committed into a project's git history, then collide on merge** ([#3427](https://github.com/bobmatnyc/trusty-tools/issues/3427)): `tm` writes composed agents (`.claude/agents/`), skills (`.claude/skills/`), and output styles (`.claude/output-styles/`) directly into a project's working tree; if any of those paths was ever committed, the next session's regeneration wrote the same path back as local content, and a later `git merge --ff-only origin/main` aborted with "your local changes would be overwritten" — the verified `duettoresearch/duetto-eve-agents` reproduction (28 commits stranded, resolved only by manual `git rm -r --cached`). `prepare_session` now calls `core::scaffold_gitignore::ensure_scaffold_gitignored` on every launch, appending a small managed block naming exactly those three subtrees to `<project_dir>/.gitignore` (idempotent, and scoped narrowly so `.claude/settings.json` / the project `.mcp.json` are never swept up). A new `scaffold_tracking` `tm doctor` check detects a project that already committed these paths — computing the true intersection of `git ls-files`-tracked scaffolding paths and the paths tm's own manifests say it regenerates — and reports the exact colliding paths plus a copy-pasteable `git rm -r --cached` remediation; it never mutates the git index itself.
- **Tests littered the operator's REAL `~/trusty-mpm-projects` root with bare-repo scaffolds** ([#3450](https://github.com/bobmatnyc/trusty-tools/issues/3450)): `tests_behavior_b_tests.rs`'s `guided_fallback_blocks_github_git_from_subdirectory` never overrode `TRUSTY_MPM_REPOS_ROOT`, so it deterministically deposited a `trusty-ci-nonexistent/subdir-test-1724(.old-layout-backup-*)` directory into the operator's real projects root on every run, on every machine. Separately, `connectors/tm_tests.rs`'s `create_session_full_lifecycle` (added after #3382/#3390's hermetic-temp-dir sweep, so never covered by it) drove the real daemon's `spawn_managed` handler with a `file://` scratch repo URL; `parse_github_path` treats any multi-segment URL as an owner/repo pair, laundering the scratch tempdir's own random name into "owner", and the handler independently reloads `TrustyToolsConfig::load()` — bypassing the test daemon's isolated root entirely — landing a real `origin/.base` bare-checkout + worktree directly in `~/trusty-mpm-projects`. Both are now hermetic: the CLI test pre-creates a stub base clone under an isolated `TRUSTY_MPM_REPOS_ROOT`, and the connector test points `TRUSTY_MPM_WORKSPACE_ROOT` at the same hermetic root its `DaemonState` already uses. `connectors/tm_tests.rs` and the duplicated `client/proxy/tests.rs` `spawn_test_daemon` helper also no longer use a bare `tempfile::tempdir().unwrap().keep()` (which both honored an inherited `$TMPDIR` and permanently disabled cleanup) — both now use `test_support::hermetic_temp_dir()` and return the guard to the caller.
- **`tm doctor`'s `output_style` check ignored `settings.local.json`, missing stale legacy `outputStyle` ids** ([#3453](https://github.com/bobmatnyc/trusty-tools/issues/3453)): `check_output_style` only ever resolved `settings.json` at the project/user scopes, never the `.local.json` variant Claude Code actually applies AHEAD of the plain file at the same scope — so a stale `outputStyle` id (e.g. the pre-rebrand `claude_mpm`, which resolves to no deployed style file) left behind in `settings.local.json` silently won and fell back to Claude Code's built-in default, while `tm doctor` reported `output_style: Ok` because it never read the file actually in effect. The check now walks the full precedence chain Claude Code uses — `project-local > project > user-local > user` — reusing the existing `ClaudeConfigReader` path model (now home-injectable via `paths_for_project_with_home`) rather than a second, divergent path join. A new `output_style_legacy_ids` doctor check additionally (Warn-only, never auto-migrating) scans every settings layer — not just the effective one — for a legacy/unresolvable id sitting dormant in a currently-shadowed layer, since tm's config-seed/repair path rewrites `settings.json` but never touches the often-gitignored `settings.local.json`.
- **`InstallPolicy` was defined but never honored by the installer** ([#3381](https://github.com/bobmatnyc/trusty-tools/issues/3381)): `install_to()` applied the same "skip if present unless `--force`" rule to every bundled artifact regardless of its declared `InstallPolicy`, so upgrades silently skipped refreshing any framework-owned file a user had touched. The enum is restored to two variants — `Overwrite` (framework-owned; all ~178 current entries) always refreshes on `tm install`, no `--force` needed; `SeedOnce` (reserved for a genuinely user-owned artifact) writes once and is never clobbered by an upgrade. `--force` now means "reset a seed-once artifact back to the shipped default" rather than "overwrite anything that exists" — its CLI help text is updated accordingly. As a side effect, this also closes a related staleness hazard: on non-dev installs, `install_to()` populates the `~/.trusty-mpm/framework/skills` fallback that `check_skill_staleness` diffs against, so the old skip-on-upgrade behavior could leave that source stale and make the staleness check return a false `Ok`; `Overwrite` now keeps it current. (This change does **not** touch `tm doctor`'s `skill_staleness` check itself, which is unrelated — it diffs the skill source directory against the deploy-time manifest and never reads `bundle::ALL`.)

## [0.19.27] — 2026-07-19

### Fixed

- **Framework-guaranteed conventions delivered only via user-editable channels** ([#3374](https://github.com/bobmatnyc/trusty-tools/issues/3374)): the commit/PR attribution footer, the proportional-documentation policy, and the ticket-attribution-at-change-site convention are now stated in the non-overridable `BASE_PM.md` floor — the one channel `resolve_pm_prompt` appends unconditionally in every override branch. Previously the proportional-documentation and ticket-attribution conventions lived only in the bundled `documentation-style` skill (user-editable; the deployer skips re-syncing a locally modified skill) and were guaranteed nowhere; the attribution footer was duplicated across three non-floor locations including a dead, never-consumed `assets/instructions/CLAUDE.md` stub. That dead asset (and its `CLAUDE_STUB` constant, `bundle_all.rs` `SeedOnce` registration, and `paths.rs` `claude_stub()` accessor) has been removed. `assets/skills/documentation-style.md` and the seeded project `CLAUDE.md` stub now point back to `BASE_PM.md` as the source of truth. A new CI guard (`scripts/check_instruction_floor.sh` + `.github/workflows/instruction-floor-guard.yml`) mechanically enforces that the floor keeps carrying these conventions and that the dead asset never reappears. ([#3377](https://github.com/bobmatnyc/trusty-tools/pull/3377))

### Changed

- `tm doctor`'s `skill_staleness` check now escalates to `Fail` (instead of `Warn`) when a **conventions-bearing** bundled skill has drifted from its bundled source — `documentation-style`, `tm-pr-workflow`, or `tm-git-file-tracking`, the small allowlist of skills whose text is the sole carrier of a framework-guaranteed rule (e.g. the commit/PR attribution footer). Ordinary skill drift stays `Warn` as before; the escalated message names the drifted skill(s) and points at `tm install` to restore the bundled version. This hardens the exact silent-drift path behind the PR #2825 attribution bypass (issue [#2825](https://github.com/bobmatnyc/trusty-tools/issues/2825)) without blocking session start — the escalation is a doctor signal only. ([#3376](https://github.com/bobmatnyc/trusty-tools/pull/3376))
- `tm`'s launch/connect splash art now sources from the shared `trusty_common::banner::TRUSTY_SPLASH_ART` / `shade_bucket` instead of a locally embedded `image.txt` + `shade_bucket` copy, so it can never silently drift from `trusty-agents`' REPL splash again (issue [#3326](https://github.com/bobmatnyc/trusty-tools/issues/3326)). Byte-identical art and rendering — `tm`'s own banner is unchanged; all 41 existing banner unit tests pass unmodified. ([#3345](https://github.com/bobmatnyc/trusty-tools/pull/3345))

### Removed

- **`tm daemon --tailscale`'s secondary listener, under ADR-0011's
  loopback-only doctrine** (closes
  [#3330](https://github.com/bobmatnyc/trusty-tools/issues/3330), part of epic
  [#3328](https://github.com/bobmatnyc/trusty-tools/issues/3328)): the daemon
  no longer binds a second, non-loopback listener for Tailscale exposure —
  `trusty-console` is the sole supported non-loopback HTTP surface, via its own
  `--tailscale`/Funnel modes. Chosen approach: **hard deprecation**, not a
  clap-level removal — the `--tailscale` flag is still parsed so old
  invocations (scripts, launchd plists) get an actionable startup error
  ("use trusty-console --tailscale; see ADR-0011") instead of a silently
  ignored flag or a generic clap parse failure. `spawn_secondary_listener`
  and `get_tailscale_ip` were removed from `daemon/mod.rs` and
  `commands/daemon_run.rs`; the router-wide write guard (#3304) now always
  trusts only the loopback default (there is no longer a second bind address
  to additionally trust). The daemon lock file no longer carries the optional
  `tailscale_addr` field (`daemon::lock::write_lock` dropped that parameter). ([#3342](https://github.com/bobmatnyc/trusty-tools/pull/3342))

## [0.19.26] — 2026-07-19

### Security

- **Daemon write guard extended from single-handler to router-wide**
  ([#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304), PR [#3317](https://github.com/bobmatnyc/trusty-tools/pull/3317)): previously
  only `coordinator_chat` (`POST /api/v1/sessions/chat`) was origin-checked;
  every other destructive route (`POST /sessions` spawn, `DELETE /sessions/{id}`,
  `POST /api/v1/control/sessions/{id}/stop`, managed-session mutation,
  `/claude-config/apply`, `/pair/*`) was exposed to cross-origin CSRF. The shared
  `trusty_common` same-origin guard is now layered router-wide at the serve site
  (`daemon::api::origin_guard::guard_router`), method-gated and fail-open on a
  missing `Origin`. The `coordinator_chat` handler keeps its own finer-grained
  origin check and `/rpc` keeps its loopback `ConnectInfo` gate (belt and
  braces).

### Added

- WorkstreamConnector trait + tm/tcode backends (twin Phase 1, DOC-44) ([#3194](https://github.com/bobmatnyc/trusty-tools/pull/3194)) ([`6890b62`](https://github.com/bobmatnyc/trusty-tools/commit/6890b624f0e772827e6f1e50f2d885f36276c3db))
- `tm sessions rename <id-or-name> <new-name>` (and in-session `tm sessions rename <new-name>` via `$TM_MANAGED_SESSION_ID`) renames a managed session, updating the record's `tmux_name` and the live tmux session, with collision + invalid-name guards ([#3302](https://github.com/bobmatnyc/trusty-tools/pull/3302))
- `tm sessions delete` now marks a session `--deleted--` (a new persisted `Deleted` state shown in the master list) instead of silently dropping the record; `tm sessions prune --state deleted` compacts the tombstones ([#3302](https://github.com/bobmatnyc/trusty-tools/pull/3302))
- `tm ls` / the session picker no longer report running/attached sessions as `(stopped)`: the list handler now reconciles each session's displayed state against live tmux (`active`/`attached` when the tmux session exists, `stopped` only when it is truly gone) so the offered action (connect vs restart) matches reality ([#3302](https://github.com/bobmatnyc/trusty-tools/pull/3302))

### Fixed

- disclaim the tmux-pane `claude` off the shared tmux server (via an `internal-spawn-disclaimed` launch shim) so TCC attributes its data-access prompts to Claude Code itself, not tmux; adds a `tcc_taint` doctor check surfacing the disclaim state (refs #2997) ([#3314](https://github.com/bobmatnyc/trusty-tools/pull/3314)) ([`7849296`](https://github.com/bobmatnyc/trusty-tools/commit/7849296bb1e26d4ba8a594289c0316b8c6700e83))
- disclaim TCC responsibility on the TUI health screen's `[S]`-key `cargo run` spawn — the last undisclaimed spawn site in the TUI/session-launch call graph from the #2997 sweep; two remaining sites elsewhere in the crate tracked as #2997 part 6 (closes #3126) ([#3261](https://github.com/bobmatnyc/trusty-tools/pull/3261)) ([`6dc1ef7`](https://github.com/bobmatnyc/trusty-tools/commit/6dc1ef753a2f5655d262850e981ea58797bd35ee))
- repair 10 dangling doc-comment pointers (unblock main) ([#3282](https://github.com/bobmatnyc/trusty-tools/pull/3282)) ([`f906005`](https://github.com/bobmatnyc/trusty-tools/commit/f906005cf9972fcdd150865219042096b03b8950))

### Changed

- extract gh_account enforcement into `gh_account_enforce.rs` and split `claude_code.rs` command-builder tests into sibling `claude_code_tests.rs`, bringing both production files back under the 500-SLOC cap (closes #3070) ([#3281](https://github.com/bobmatnyc/trusty-tools/pull/3281)) ([`83d3ed3`](https://github.com/bobmatnyc/trusty-tools/commit/83d3ed3e2aef4d528af67a0b123f5d985f660072))
- tm-adr skill v2.0.0 — formalize ADRs as first-class documentation artifact (opt-in → mandatory) ([#3172](https://github.com/bobmatnyc/trusty-tools/pull/3172))

### Documentation

- proportional documentation policy — lighter touch for self-evident code ([#3271](https://github.com/bobmatnyc/trusty-tools/pull/3271)) ([`5bc8aba`](https://github.com/bobmatnyc/trusty-tools/commit/5bc8abab497b1993a72d731d361316c086cabc73))
- DOC-46 — formalize ADRs as first-class artifact with consistency-vetting protocol ([#3172](https://github.com/bobmatnyc/trusty-tools/pull/3172)) ([`b775ede`](https://github.com/bobmatnyc/trusty-tools/commit/b775edebcd2a1ee7a11396ebc392d69b8ab9defc))
- in-flight issue/PR progress updates as standard PM workflow convention: bug diagnoses at triage time, progress comments at meaningful state changes, PR body freshness, and completion evidence (closes #3149) ([#3151](https://github.com/bobmatnyc/trusty-tools/pull/3151))

## [0.19.25] — 2026-07-18

### Added

- daemon startup version banner + stale-daemon doctor check ([#2968](https://github.com/bobmatnyc/trusty-tools/pull/2968)) ([`abbcc45`](https://github.com/bobmatnyc/trusty-tools/commit/abbcc45c646cf821e8a9ed0828e66b11fc9b0a94))
- per-session asset staleness + tm sessions sync-assets ([#2980](https://github.com/bobmatnyc/trusty-tools/pull/2980)) ([`c8d137a`](https://github.com/bobmatnyc/trusty-tools/commit/c8d137a632275e85709e745176a39253e62d6e3b))
- bridge custom remote+local MCP servers into fleet sessions at project/user scope ([#3033](https://github.com/bobmatnyc/trusty-tools/pull/3033)) ([`07cb73f`](https://github.com/bobmatnyc/trusty-tools/commit/07cb73f6bc236f753bb8e920ebab20b5cd9658c5))
- per-project gh account pinning via GH_TOKEN spawn-env injection (closes #3025) ([#3040](https://github.com/bobmatnyc/trusty-tools/pull/3040)) ([`53d0a54`](https://github.com/bobmatnyc/trusty-tools/commit/53d0a5433c9e42b2c49f11f943de8cfc2c7f4696))

### Fixed

- entry-level hook matching for mixed groups + lifecycle triad in project-tier settings ([#2969](https://github.com/bobmatnyc/trusty-tools/pull/2969)) ([`de11649`](https://github.com/bobmatnyc/trusty-tools/commit/de11649700a79028771092cc22ea6343e4a8da84))
- explicit --url errors on unreachable daemon instead of silent fallback ([#2971](https://github.com/bobmatnyc/trusty-tools/pull/2971)) ([`c228ad9`](https://github.com/bobmatnyc/trusty-tools/commit/c228ad9a31093f79c27866d7ae15f0cb253e710f))
- tm doctor output_style content-drift + orphan detection ([#2976](https://github.com/bobmatnyc/trusty-tools/pull/2976)) ([`05a4a05`](https://github.com/bobmatnyc/trusty-tools/commit/05a4a059b1cd4d00f052e5a504141b8f2b621b84))
- write_project_hooks uses write_json_atomic for torn-write parity (closes #2972) ([#3018](https://github.com/bobmatnyc/trusty-tools/pull/3018)) ([`32e805d`](https://github.com/bobmatnyc/trusty-tools/commit/32e805d609e15bbcc02ab4212f606e0e34b20293))
- teach user-scope MCP registration via tm mcp add in tm-cli-operations (closes #3020) ([#3021](https://github.com/bobmatnyc/trusty-tools/pull/3021)) ([`f6223aa`](https://github.com/bobmatnyc/trusty-tools/commit/f6223aacc1c030055440cea93c5ae52c1c4d231f))
- disclaim TCC responsibility on session-launch spawns so Claude Code isn't attributed to trusty-mpm (closes #2997) ([#3037](https://github.com/bobmatnyc/trusty-tools/pull/3037)) ([`d481a0c`](https://github.com/bobmatnyc/trusty-tools/commit/d481a0cd7e264b20109e1c92122ba972db828222))

### Changed

- single shared tmux library; route trusty-mpm + trusty-agents through it ([#3017](https://github.com/bobmatnyc/trusty-tools/pull/3017)) ([`383b9f4`](https://github.com/bobmatnyc/trusty-tools/commit/383b9f475e781ef6049900f1630875e8ebf68264))

### Documentation

- long-wait protocol — disarm monitors on goal completion ([#2960](https://github.com/bobmatnyc/trusty-tools/pull/2960)) ([`e193414`](https://github.com/bobmatnyc/trusty-tools/commit/e1934140d38d6d9b847fef07676f29277683b220))

## [0.19.24] — 2026-07-17

### Added

- pm_guard — 3-per-turn file-change budget, read-only-redirection fix, content-aware routing hints (closes #2918, refs #2745) ([#2928](https://github.com/bobmatnyc/trusty-tools/pull/2928)) ([`95b69c8`](https://github.com/bobmatnyc/trusty-tools/commit/95b69c80e08e396de8da7fe606d329e8360835a3))
- rust-build-performance bundled skill — dev-loop + dependency-graph build hygiene, declared for the rust-engineer and tauri-engineer agents ([#2929](https://github.com/bobmatnyc/trusty-tools/pull/2929)) ([`c278874`](https://github.com/bobmatnyc/trusty-tools/commit/c278874db20e017323de27129f59647682de643f))

### Fixed

- tm hooks live only in the tm-owned settings path — stop `tm install` from discovering and mutating every `.claude/settings.json`/`settings.local.json` under `$HOME`, adds `tm hooks clean` + a doctor check ([#2945](https://github.com/bobmatnyc/trusty-tools/pull/2945)) ([`3940d7f`](https://github.com/bobmatnyc/trusty-tools/commit/3940d7f90d25fd57ef02ea47bb09a20cca06d34e))

---
## [0.19.23] — 2026-07-17

### Added

- skill-port batch 1: 25 upstream `universal/` skills ported from `bobmatnyc/claude-mpm-skills` (main, 172 SKILL.md files) into trusty-mpm's bundled skill catalog (93 files, ~993 KB — 25 entry `SKILL.md` + 68 `references/*.md`), plus multi-file skill-directory deploy machinery: `bundle_all.rs` gains an `overwrite()` shorthand constructor so the ~164-entry `ALL` table stays under the SLOC cap; `skill_deployer.rs`'s `deploy_skills_filtered` now mirrors each stem's `references/*.md` files alongside its entry point via a shared `deploy_one_file` helper; `skill_source.rs`'s `materialize_skill_artifacts` writes nested rel_paths under a matching nested directory and a new recursive `prune_orphaned_skill_files` removes now-empty directories. Restores `skills:` frontmatter declarations on all 36 upstream-matched bundled agents, computed by unioning each agent's upstream `skills:` frontmatter (alias-resolved via the epic's 13-entry table) intersected with the 25 ported names (refs epic #2902, closes #2903)
- `tm generate capabilities[--check]` and the auto-generated `tm-capabilities` bundled skill: a dev-time subcommand walks the CLI command tree (clap introspection), the MCP tool catalog, the bundled agent roster, the bundled skill catalog, and a maintained-and-cross-checked doctor-check list, then writes a committed, byte-reproducible reference catalog (`skills/tm-capabilities.md` + 5 generated `references/*.md` files, plus one hand-authored `references/workflows.md` covering session launch / delegation / doctor triage / bug-report end-to-end flows). `--check` is the CI drift gate (`scripts/check_capabilities.sh`, wired into `.github/workflows/capabilities-drift.yml`) — a stale committed file fails the build instead of silently rotting. `BASE-AGENT.md` declares `skills: [tm-capabilities]` so every one of the 37 concrete agents picks it up via the existing DOC-42 union-across-chain compose merge; the existing `tm` skill gains one pointer line to it (complement, not supersede). Also fixes a latent bug in `materialize_skill_artifacts`'s mcp-shadow-guard stem computation that incorrectly widened the check to a multi-file skill's full nested `references/*.md` path instead of just the skill's own name (closes #2913)
- bundled `documentation-style` skill: a two-tier (`SKILL.md` + 6 `references/*.md`) SLD-grounded style guide covering the four-axis Why/What/Test + opt-in Spec-References inline doc model, per-artifact-type conventions (spec, README, file-level, class/module, method/function, block/inline), and context-economy guidance — defers to DOC-38 for the actual reference grammar rather than restating it (Annex B follow-up F2). `BASE-ENGINEER.md`'s Deliverables Checklist now points engineers at it, and declares `skills: [documentation-style]` in frontmatter so every engineer-family agent picks it up via the existing DOC-42 union-across-chain compose merge (closes #2911)

### Changed

- extract skills deploy/manifest/tiers machinery from trusty-mpm (refs #2892, #2818) ([#2916](https://github.com/bobmatnyc/trusty-tools/pull/2916)) ([`488602d`](https://github.com/bobmatnyc/trusty-tools/commit/488602dfa5cc75916f33c66b555832ce310b0025))
- extract agent compose/deploy/manifest machinery from trusty-mpm (refs #2892) ([#2909](https://github.com/bobmatnyc/trusty-tools/pull/2909)) ([`bb947ea`](https://github.com/bobmatnyc/trusty-tools/commit/bb947ead9e220a37b8902b1190d261295c23538b))

## [0.19.22] — 2026-07-17

### Added

- agent-bundled skills mechanism (DOC-42, [`docs/specs/agent-bundled-skills.md`](../../docs/specs/agent-bundled-skills.md)): agents declare a `skills:` frontmatter list (block- and inline-style YAML both parse), the deploy pipeline co-deploys each referenced skill alongside its owning agent, `tm doctor` gains two new checks (`agent_skills` for dangling skill references, can Warn; `agent_skills_prose_hints`, always informational) bringing the doctor suite to 15 checks, and `tm agent list` / `tm agent show <agent>` surface each agent's resolved skill tier in the CLI. Per-agent deploy-failure isolation: `deploy_agents_filtered` no longer aborts the whole roster on one agent's compose failure — it logs the failure, records it in `DeployResult.failed`, and continues deploying the rest of the roster (closes [#2889](https://github.com/bobmatnyc/trusty-tools/issues/2889)) ([#2906](https://github.com/bobmatnyc/trusty-tools/pull/2906)) ([`4e4a2b9`](https://github.com/bobmatnyc/trusty-tools/commit/4e4a2b993547afef8b15bf29365163186a93f14f))

### Fixed

- restore code-critic's upstream review content as two bundled skills — `code-review-standards` (severity taxonomy, 80% confidence filter, APPROVE/WARN/BLOCK verdict protocol) and `contract-driven-testing` (three-level test pyramid derived from Code Contracts) — with the `code-critic` agent's `skills:` frontmatter updated to reference both (closes [#2890](https://github.com/bobmatnyc/trusty-tools/issues/2890)) ([#2900](https://github.com/bobmatnyc/trusty-tools/pull/2900)) ([`5214cd7`](https://github.com/bobmatnyc/trusty-tools/commit/5214cd700a48f4f688f8d596a40b822a37a6df4b))

### Fixed

- PM relays operator authority — scope injection-skepticism to untrusted content, not the dispatching PM ([#2844](https://github.com/bobmatnyc/trusty-tools/pull/2844)) ([`f0614b2`](https://github.com/bobmatnyc/trusty-tools/commit/f0614b293eb1fe0dbca6b20b093422b7cd608809))
- idle-park mitigation protocol for Agent-tool subagents ([#2835](https://github.com/bobmatnyc/trusty-tools/pull/2835)) ([`91f7c4d`](https://github.com/bobmatnyc/trusty-tools/commit/91f7c4d4d3fe6cc66c377b30ccff3a5425f96db5))

### Added

- idle-park mitigation protocol for in-conversation Agent-tool subagents — the layer below #2621's managed-session nudge, which cannot reach a subagent (no tmux pane to inject into). Amends the bundled prompt assets in their existing style: BASE-AGENT.md's "Foreground Execution" section gains a chunked-repoll subsection that also forbids the OPPOSITE failure (a 30-second blind-poll spam loop) — prefer the silent blocking `--watch`, size any manual sleep to the real wall-clock, message only on state change, one-shot `gh run view` diagnosis on overrun; the `version-control` and `local-ops` personas get a matching anti-spam bullet; PM_INSTRUCTIONS.md adds a "Parked-Subagent Detection & Nudge" protocol (recognize a parked stop by unmet-goal + backgrounded-wait language, `SendMessage` the SAME agent a resume nudge, prefer crate-scoped gates and blocking `--watch` to keep waits under the 10-min tool ceiling, never nudge a genuine human-wait); and `tm-delegation-patterns` documents the long-wait/chunked-repoll delegation pattern as first-class. Prompt/asset-only — the daemon-side hook for in-conversation subagents is deferred as a follow-up proposal (closes #2833)
- bundled `dotnet-engineer` agent — a modern C#/.NET 8+ specialist (ASP.NET Core, EF Core, xUnit, nullable reference types, minimal APIs, `dotnet` CLI workflow) with explicit legacy VB.NET awareness (reads/maintains `.vbproj`, WinForms/WebForms code; recommends interop over blind rewrites). Wired into per-project stack detection via new `*.sln`/`*.csproj`/`*.vbproj`/`global.json`/`Directory.Build.props` markers (extension-glob marker support added to `project_lang`), so the auto-derived `## Detected Project Stack` section and agent-roster scoping route C#/.NET/VB.NET projects to the specialist instead of the general-purpose fallback (closes #2831)
- project- and user-level custom skill tiers with precedence project-custom > user-custom > bundled: skills authored in `~/.trusty-mpm/skills/` deploy into every project and override same-named bundled skills, while a skill hand-placed in a project's `.claude/skills/` outranks both and is never overwritten on redeploy; collisions are logged. New `core::skill_tiers` (pure `plan_skill_tiers` planner + `deploy_all_skill_tiers` orchestrator) and `FrameworkPaths::user_skill_source_dir()`; `tm catalog apply`, `tm install`, and both tm-global `CLAUDE_CONFIG_DIR` bootstrap paths (`managed_config`, `standalone::global_config`) now route through the same multi-tier deploy so a user-tier override survives a catalog refresh, a routine reinstall, or a session-config rebuild — every raw single-tier `deploy_skills`/`deploy_skills_filtered` call site outside `skill_deployer.rs` itself has been migrated; documented in `PM_INSTRUCTIONS.md` and the `mpm-skills-manager` agent (closes #2816)
- auto-nudge idle-parked managed sessions ([#2781](https://github.com/bobmatnyc/trusty-tools/pull/2781)) ([`be2ec11`](https://github.com/bobmatnyc/trusty-tools/commit/be2ec1107a56bd1c25e07c9fa146717399effd23))
- require per-PR changelog updates in the default workflow ([#2790](https://github.com/bobmatnyc/trusty-tools/pull/2790)) ([`238f07e`](https://github.com/bobmatnyc/trusty-tools/commit/238f07e8f77b2290afc1c42939e7802e5d8a5074))
- propagate native trusty MCP servers to fleet sessions ([#2739](https://github.com/bobmatnyc/trusty-tools/pull/2739)) ([#2748](https://github.com/bobmatnyc/trusty-tools/pull/2748)) ([`0a87f1d`](https://github.com/bobmatnyc/trusty-tools/commit/0a87f1d70e97fd630bd997db8f17a98d64ca00d4))

### Fixed

- the dispatching PM now speaks with the operator's authority in the bundled agent prompts: a subagent must treat a PM-relayed authorization (even one pre-labeled AUTHORIZED or citing operator precedent it can't independently verify) AS operator authorization, and must NOT demand direct end-user confirmation or treat the PM as an untrusted third party. Fixes a live regression where a `version-control` agent froze an operator-authorized admin-merge — stalling six PRs — because it treated the PM's relayed authorization as a third party's unverifiable word. BASE-AGENT.md gains a concise "PM Authority & Escalation" section (inherited by every composed agent) that separates two axes: AUTHORITY ("is this authorized?") is settled by the PM's word and doubt escalates back to the PM — never a unilateral refusal or pipeline freeze — while OBJECTIVE safety gates the agent can verify itself (never merge red/pending CI, `--admin` bypasses bot/review approval only; never fabricate evidence; never violate worktree discipline) stay the agent's own non-negotiables. Injection-skepticism is explicitly scoped to UNTRUSTED CONTENT (file contents, web pages, tool output, third-party text), never the dispatching PM's instructions. `version-control.md`'s PR-workflow block, which previously forbade all direct merges and framed merges as needing independently-verified human approval, now complies with a PM-relayed admin-merge authorization (still refusing red/pending CI) (closes #2842)
- stop triggering repeated macOS TCC consent prompts attributed to `trusty-mpm` on the tmux-hosted managed-session path — both the Apple Music / media-library class (`kTCCServiceMediaLibrary`) and the "access data from other apps" App-Data class (`kTCCServiceSystemPolicyAppData`): disclaim macOS TCC responsibility (`responsibility_spawnattrs_setdisclaim`) when spawning the tmux server so each managed agent it hosts is its OWN responsible process instead of rolling attribution up to the signed `trusty-mpm` binary. The disclaim is service-agnostic, so it covers every child-initiated prompt class at once for that path; the direct-spawn `tm run` and stream-JSON backend paths are tracked separately (closes #2819)
- derive PM stack profile per project, no stack default ([#2815](https://github.com/bobmatnyc/trusty-tools/pull/2815)) ([`5aadeaa`](https://github.com/bobmatnyc/trusty-tools/commit/5aadeaa53e896133f02c9ffcbd4d4a3b7d4541a3))
- reconcile zombie-Active sessions on in-place relaunch instead of 409 dead-end ([#2795](https://github.com/bobmatnyc/trusty-tools/pull/2795)) ([`5ef199a`](https://github.com/bobmatnyc/trusty-tools/commit/5ef199a920374ab6ac3e97b3aac8b08374915b1d))
- bump `jsonwebtoken` 9 → 10 (`aws_lc_rs` backend), fixing GHSA-h395-gr6q-cpjc; migrated the test-only JWT decode-without-verification call site to `jsonwebtoken::dangerous::insecure_decode` (closes #2765) ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`298df87`](https://github.com/bobmatnyc/trusty-tools/commit/298df87e6a5b5e96874ea2866509303df14712bc))
- relaunch decommissioned managed session on bare tm instead of reconnect-to-self ([#2780](https://github.com/bobmatnyc/trusty-tools/pull/2780)) ([`b6f1783`](https://github.com/bobmatnyc/trusty-tools/commit/b6f1783a6e7ad78fb42c332f50ed86e2fb8e2bfd))
- PM priming no longer ships a hard-coded stack profile: the bundled output styles (`trusty-mpm`, `-research`, `-teacher`) are now stack-neutral — the "Rust workspace" declaration, `rust-engineer`-only delegation map, and `cargo`/`make check` quality gate are replaced with detect-first guidance — and every per-project PM prompt now carries an auto-derived `## Detected Project Stack` section (new `core::stack_profile`, reusing the `project_lang` marker detection) that routes to the project's actual language engineer(s) or, when nothing is detected, a neutral profile mandating a Research phase and forbidding any default. Fixes a Rust profile leaking into a Next.js/TypeScript project (ai-power-rankings); skills-side precedent #2005/#2006 (closes #1971)
- resolve managed MCP registry from real home in fleet-session injector ([#2761](https://github.com/bobmatnyc/trusty-tools/pull/2761)) ([`7c04f1f`](https://github.com/bobmatnyc/trusty-tools/commit/7c04f1f845432edd6b1fb488103a1aba9939fb3c))
- never traverse $HOME/TCC-protected folders (closes #2759) ([#2760](https://github.com/bobmatnyc/trusty-tools/pull/2760)) ([`74fe7f3`](https://github.com/bobmatnyc/trusty-tools/commit/74fe7f3c682cb5b0c648b63585c44ee47aa6d652))
- positive stdio field allowlist for fleet native-MCP injection ([#2739](https://github.com/bobmatnyc/trusty-tools/pull/2739)) ([#2755](https://github.com/bobmatnyc/trusty-tools/pull/2755)) ([`e3a5c68`](https://github.com/bobmatnyc/trusty-tools/commit/e3a5c684cc25028202f6ece1a262f515fa3b43da))

### Changed

- collapse stacked Unreleased headings (trusty-mpm, trusty-common) ([`95b16c8`](https://github.com/bobmatnyc/trusty-tools/commit/95b16c8c468084ce0e2a46eac8a4acf99cfab823))

### Documentation

- correct stale session_launch test comments ([#2779](https://github.com/bobmatnyc/trusty-tools/pull/2779)) ([`3371828`](https://github.com/bobmatnyc/trusty-tools/commit/33718282bc3e3da4da6f84ff6681d84cd2859a26))

### Added


### Added

- implement Slack send + read tools; extract SlackFormatter to trusty-common ([#2722](https://github.com/bobmatnyc/trusty-tools/pull/2722)) ([`847a0c3`](https://github.com/bobmatnyc/trusty-tools/commit/847a0c334e1a8822a7d31696b48946e538aca7cc))

### Fixed

- reconcile zombie-active sessions on in-place relaunch (closes #2743) ([#2744](https://github.com/bobmatnyc/trusty-tools/pull/2744)) ([`f1552c7`](https://github.com/bobmatnyc/trusty-tools/commit/f1552c72e4f20afda74466f97865845660d18a30))
- pm_guard recognizes git global flags and stops scanning quoted content ([#2741](https://github.com/bobmatnyc/trusty-tools/pull/2741)) ([`6e29e25`](https://github.com/bobmatnyc/trusty-tools/commit/6e29e253c073cb889bdbfce5e99d05d16e6d1fe2))
- framework workflow conventions — per-session session log, issue/PR assignee+label defaults, trusty-mpm attribution footer ([#2737](https://github.com/bobmatnyc/trusty-tools/pull/2737)) ([`3089600`](https://github.com/bobmatnyc/trusty-tools/commit/3089600b475cc464329132e5f7523536bb730797))

### Added

- shared ensure-project-indexed helper; wire tcode task start ([#2701](https://github.com/bobmatnyc/trusty-tools/pull/2701)) ([`28a8d11`](https://github.com/bobmatnyc/trusty-tools/commit/28a8d11d4a5eac21921c3ceeef8707f71cf35459))

### Changed

- DispositionReason enum replaces stringly-typed disposition reasons ([#2705](https://github.com/bobmatnyc/trusty-tools/pull/2705)) ([`e7970c8`](https://github.com/bobmatnyc/trusty-tools/commit/e7970c87541d774aa25c315f27da7fee656be492))
- convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))

### Fixed

- picker launch-new switches client safely and exits instead of hanging ([#2680](https://github.com/bobmatnyc/trusty-tools/pull/2680)) ([`3de0647`](https://github.com/bobmatnyc/trusty-tools/commit/3de0647452f0e77a67aa20c52c8d9610474b8de8))
- pm_guard allows read-only sed/awk pipe segments ([#2677](https://github.com/bobmatnyc/trusty-tools/pull/2677)) ([`3881639`](https://github.com/bobmatnyc/trusty-tools/commit/38816393f9d836b79e08c8efd4642fd21f3a6e5f))

### Fixed

- bound the manager ChatStore conversation-key set with an LRU cap ([#2648](https://github.com/bobmatnyc/trusty-tools/pull/2648)) ([`0d776f9`](https://github.com/bobmatnyc/trusty-tools/commit/0d776f9e8de429b76ac00a9da355bd76fc75b933))
- exclude dead sessions (missing workspace) from resume/restart offers ([#2652](https://github.com/bobmatnyc/trusty-tools/pull/2652)) ([`4a8870f`](https://github.com/bobmatnyc/trusty-tools/commit/4a8870fb2990ca604f84da4ed7d46ac2203e11e4))
- tm sessions resume hands terminal to the resumed session's tmux window ([#2656](https://github.com/bobmatnyc/trusty-tools/pull/2656)) ([`6f260cd`](https://github.com/bobmatnyc/trusty-tools/commit/6f260cde3e58849278b00a6160b3ed1e3a116a33))
- sync session worktrees on resume + de-duplicate PM mandate injection ([#2653](https://github.com/bobmatnyc/trusty-tools/pull/2653)) ([`364c71c`](https://github.com/bobmatnyc/trusty-tools/commit/364c71cadb8ec0adc85ea36ea2ae70ef56a436a5))

### Changed

- split bin/tm/cli.rs into cli/ modules ([#2650](https://github.com/bobmatnyc/trusty-tools/pull/2650)) ([`53eb4a6`](https://github.com/bobmatnyc/trusty-tools/commit/53eb4a67f3cec9ba4f9d23ddf65243d20974d5ec))

### Added

- tm-manager phase 2 — route-task, proposal-and-confirm, route CLI (#2585 #2586 #2587) ([#2615](https://github.com/bobmatnyc/trusty-tools/pull/2615)) ([`87e4d1d`](https://github.com/bobmatnyc/trusty-tools/commit/87e4d1d4851b218bc651a2bb35ae681923633dfd))

### Fixed

- harden agents against idle parking — never end a turn to wait (closes #2610) ([#2620](https://github.com/bobmatnyc/trusty-tools/pull/2620)) ([`cbcfd17`](https://github.com/bobmatnyc/trusty-tools/commit/cbcfd17e797c6db80074e1254dfefb5f1f8fb3c1))

### Added

- async managed-spawn provisioning with live progress poll route (closes #2605) ([#2607](https://github.com/bobmatnyc/trusty-tools/pull/2607)) ([`f440192`](https://github.com/bobmatnyc/trusty-tools/commit/f44019266928e96973d16ab83cc618a4ac0894ff))
- tm manager status|digest|chat CLI (TMMGR phase 1) ([#2600](https://github.com/bobmatnyc/trusty-tools/pull/2600)) ([`ed9390a`](https://github.com/bobmatnyc/trusty-tools/commit/ed9390a8a0f3e491f2aa6416cf68b5603975536a))
- tm manager phase-1b — digest, read-only chat, hermetic suite ([#2601](https://github.com/bobmatnyc/trusty-tools/pull/2601)) ([`6f80f36`](https://github.com/bobmatnyc/trusty-tools/commit/6f80f36999c66ff679f194fe3e2d3bd9b3214c79))

### Fixed

- pm_guard permits PM single-file writes to non-source paths ([#2606](https://github.com/bobmatnyc/trusty-tools/pull/2606)) ([`3c6f9f7`](https://github.com/bobmatnyc/trusty-tools/commit/3c6f9f705760f1e94a37f6b9d966b45bf483c2ed))

### Added

- tm manager phase-1a — scaffold, status rollup, portfolio palace ([#2598](https://github.com/bobmatnyc/trusty-tools/pull/2598)) ([`3084a3c`](https://github.com/bobmatnyc/trusty-tools/commit/3084a3c406c2aa935c4c736950b81954c47fea39))

### Fixed

- session restart returns 500 for stopped session whose workspace is gone (closes #2577) ([#2594](https://github.com/bobmatnyc/trusty-tools/pull/2594)) ([`f2270f1`](https://github.com/bobmatnyc/trusty-tools/commit/f2270f1f2effb417aaf460f21b56c311d3403b34))

### Added

- task-injection delivery observability + readiness-probe hardening (closes #2364) ([#2568](https://github.com/bobmatnyc/trusty-tools/pull/2568)) ([`d0fe72b`](https://github.com/bobmatnyc/trusty-tools/commit/d0fe72b943434a4adab735e1973c1446f067f1a0))
- wire Slack onto the channel-agnostic SessionProxy (closes #2549) ([#2565](https://github.com/bobmatnyc/trusty-tools/pull/2565)) ([`26aa1ae`](https://github.com/bobmatnyc/trusty-tools/commit/26aa1ae5e6a4d890221874d181037180bb9cfde2))
- expose SessionProxy focus/inject/summarize as MCP tools ([#2562](https://github.com/bobmatnyc/trusty-tools/pull/2562)) ([`4edcc3b`](https://github.com/bobmatnyc/trusty-tools/commit/4edcc3bba3a848e9dba4d97401ee370423edf92f))

### Changed

- retire duplicate Bedrock ports onto trusty-common::inference ([#2567](https://github.com/bobmatnyc/trusty-tools/pull/2567)) ([`31c9fc0`](https://github.com/bobmatnyc/trusty-tools/commit/31c9fc0467adfda65b702c7433ceded01e0cf884))

### Fixed

- reconcile agent roster drift across deploy destinations ([#2547](https://github.com/bobmatnyc/trusty-tools/pull/2547)) ([`8d8b042`](https://github.com/bobmatnyc/trusty-tools/commit/8d8b04230a646f8c7941754c7cd4684f57d32089))
- pane-scope session capture reads to the recorded harness pane ([#2545](https://github.com/bobmatnyc/trusty-tools/pull/2545)) ([`18bf5b2`](https://github.com/bobmatnyc/trusty-tools/commit/18bf5b28f8ee47491c3660593195b5ca02ec8883))

### Fixed

- guided-default cwd's own repo wins over an ancestor ([#2542](https://github.com/bobmatnyc/trusty-tools/pull/2542)) ([#2543](https://github.com/bobmatnyc/trusty-tools/pull/2543)) ([`51f5041`](https://github.com/bobmatnyc/trusty-tools/commit/51f50417e23c76a4bea4beabc854c4e75cacd526))

### Added

- Test-pointer lint gate (closes #2458) ([#2529](https://github.com/bobmatnyc/trusty-tools/pull/2529)) ([`9876b44`](https://github.com/bobmatnyc/trusty-tools/commit/9876b44d72d2a68dd9df213e027d2f15a119e2b9))
- deterministic project configurator — tm projects config CLI + TUI form ([#2484](https://github.com/bobmatnyc/trusty-tools/pull/2484)) ([`bfb58a6`](https://github.com/bobmatnyc/trusty-tools/commit/bfb58a61db392eea868828492b532dde9bf41750))
- PATCH /api/v1/projects/{name} — registry-B field-level config update ([#2481](https://github.com/bobmatnyc/trusty-tools/pull/2481)) ([`6e2cc38`](https://github.com/bobmatnyc/trusty-tools/commit/6e2cc38d2fb91dddc001de5c2b7dccd470564481))
- TUI Deliverable glyph + read-only Deliverable/Milestone view ([#2473](https://github.com/bobmatnyc/trusty-tools/pull/2473)) ([`fed11da`](https://github.com/bobmatnyc/trusty-tools/commit/fed11da19b3e05935363228882c6885dd014d00f))
- TUI live-refresh + activity-pane wiring ([#2469](https://github.com/bobmatnyc/trusty-tools/pull/2469)) ([`7430bd8`](https://github.com/bobmatnyc/trusty-tools/commit/7430bd877e8d692af2bdbc9be1f695e5433652e5))
- multipane TUI skeleton — 4-pane projects control plane ([#2465](https://github.com/bobmatnyc/trusty-tools/pull/2465)) ([`ea89558`](https://github.com/bobmatnyc/trusty-tools/commit/ea8955859b36b6182bc0d047c1e465584a9a6049))
- SessionRecord.deliverable_id + --deliverable launch wiring (WI-13, closes #2379) ([#2439](https://github.com/bobmatnyc/trusty-tools/pull/2439)) ([`0c4799b`](https://github.com/bobmatnyc/trusty-tools/commit/0c4799be8914098d9d60063b3dfaf95c27d0fdd7))
- deliverable/milestone status histograms on project status endpoint ([#2429](https://github.com/bobmatnyc/trusty-tools/pull/2429)) ([`d92dd67`](https://github.com/bobmatnyc/trusty-tools/commit/d92dd67b013756316fe4b03e24ea3c6a3c9f881b))
- tm projects CLI — list/register/show/status + deliverables/milestones subtrees (closes #2115, #2381) ([#2428](https://github.com/bobmatnyc/trusty-tools/pull/2428)) ([`cca7216`](https://github.com/bobmatnyc/trusty-tools/commit/cca72166d9eaeabc24b8097fdebdd7589ace0e64))
- common tmux entry point + generous scrollback ([#2399](https://github.com/bobmatnyc/trusty-tools/pull/2399)) ([`2651fb3`](https://github.com/bobmatnyc/trusty-tools/commit/2651fb33622a077fa4323238daf5ad24484105bc))
- Deliverable/Milestone data model, central stores, CRUD API + state-machine enforcement (closes #2378, #2380) ([#2395](https://github.com/bobmatnyc/trusty-tools/pull/2395)) ([`359fb79`](https://github.com/bobmatnyc/trusty-tools/commit/359fb797d1fe73917c34f8d5be869c54aa3d22a0))
- canonical tm sessions namespace with deprecated tm session alias ([#2394](https://github.com/bobmatnyc/trusty-tools/pull/2394)) ([`9b4ba83`](https://github.com/bobmatnyc/trusty-tools/commit/9b4ba83a7aec7bbb2dee5a8960e8aacf65761670))
- deterministic project status-aggregation endpoint (closes #2117) ([#2396](https://github.com/bobmatnyc/trusty-tools/pull/2396)) ([`8ec8898`](https://github.com/bobmatnyc/trusty-tools/commit/8ec88983695583230e7d4cff82ba118bfda1e0de))
- tm-issues-prune skill organizes, prioritizes, and suggests next tasks ([#2393](https://github.com/bobmatnyc/trusty-tools/pull/2393)) ([`1266097`](https://github.com/bobmatnyc/trusty-tools/commit/1266097f2c70417270aa15c58599a7e42b20cd48))
- Telegram focused-session free-text routing (TELUI-6, #1440) ([#2372](https://github.com/bobmatnyc/trusty-tools/pull/2372)) ([`362cb72`](https://github.com/bobmatnyc/trusty-tools/commit/362cb72af874f7783fd84f105eec55574b6e6db3))
- align PM identity string to tm-<project>-<n> naming (closes #2325) ([#2328](https://github.com/bobmatnyc/trusty-tools/pull/2328)) ([`e20b878`](https://github.com/bobmatnyc/trusty-tools/commit/e20b878f6cb7f6a590a0aed92bd94d29dc97b14c))
- bundled tm CLI operations skill incl. MCP management (closes #2321) ([#2323](https://github.com/bobmatnyc/trusty-tools/pull/2323)) ([`970ebde`](https://github.com/bobmatnyc/trusty-tools/commit/970ebdea59cc29a0f900ad5dff1883cc59fb8dd3))
- tm mcp test verifies MCP servers via stdio handshake (closes #2311) ([#2316](https://github.com/bobmatnyc/trusty-tools/pull/2316)) ([`e1eb9a6`](https://github.com/bobmatnyc/trusty-tools/commit/e1eb9a657c077d41835d06c676755263fce48375))
- add delete action to interactive tm ls session picker (closes #2304) ([#2310](https://github.com/bobmatnyc/trusty-tools/pull/2310)) ([`5e0af0a`](https://github.com/bobmatnyc/trusty-tools/commit/5e0af0a8227cec3d24fbb694e702dcc63977a968))
- inject CLAUDE_CODE_OAUTH_TOKEN into managed sessions — fix login loop (closes #2246) ([#2256](https://github.com/bobmatnyc/trusty-tools/pull/2256)) ([`07a9085`](https://github.com/bobmatnyc/trusty-tools/commit/07a90857da5eba20e356326b14fac371098d4647))
- tm ls becomes session connector; alias list moves to tm ls --projects/-p ([#2297](https://github.com/bobmatnyc/trusty-tools/pull/2297)) ([`0365a1b`](https://github.com/bobmatnyc/trusty-tools/commit/0365a1bd1133a68911ec0a61ff03f76a05f18082))
- tm mcp add|remove|list for user-level MCP servers in tm config dir ([#2286](https://github.com/bobmatnyc/trusty-tools/pull/2286)) ([`a2cf543`](https://github.com/bobmatnyc/trusty-tools/commit/a2cf543feae45700b4bab8a6e6a0af64122ea2d5))

### Fixed

- guided-default no longer inherits an ancestor repo's project when cwd is untracked ([#2535](https://github.com/bobmatnyc/trusty-tools/pull/2535)) ([`9bba5ee`](https://github.com/bobmatnyc/trusty-tools/commit/9bba5eecc7a1a2b65368e4a743dca2f9436919bf))
- route tm CLI top-level client through bounded config (closes #2517) ([#2524](https://github.com/bobmatnyc/trusty-tools/pull/2524)) ([`9200bc5`](https://github.com/bobmatnyc/trusty-tools/commit/9200bc5e605070780dd78e5026dcd2d758d40bce))
- non-zero exit on session spawn failure (closes #2457) ([#2521](https://github.com/bobmatnyc/trusty-tools/pull/2521)) ([`74f95c7`](https://github.com/bobmatnyc/trusty-tools/commit/74f95c78653bc3a1ae5b220e2e8ff6b8d9877450))
- SessionEnd gate aggregates any-pane-live like the runtime reaper ([#2516](https://github.com/bobmatnyc/trusty-tools/pull/2516)) ([`e30c686`](https://github.com/bobmatnyc/trusty-tools/commit/e30c68665cca7185c5198186526eb8a84e7a56c8))
- harden inject/observe/dashboard-restart against active-pane ambiguity (closes #2468) ([#2514](https://github.com/bobmatnyc/trusty-tools/pull/2514)) ([`47b8387`](https://github.com/bobmatnyc/trusty-tools/commit/47b8387b23a8cdf2121aaf3619bcfd63be39005d))
- per-request DaemonClient timeouts + TUI input protection (closes #2471) ([#2512](https://github.com/bobmatnyc/trusty-tools/pull/2512)) ([`8ccb867`](https://github.com/bobmatnyc/trusty-tools/commit/8ccb867a152016858bd7d2643d11a606da7978c4))
- TUI activity pane — explicit unavailable state instead of perpetual loading ([#2513](https://github.com/bobmatnyc/trusty-tools/pull/2513)) ([`99ee122`](https://github.com/bobmatnyc/trusty-tools/commit/99ee122fcc680bd168c057a49f2cad01057bd31c))
- agent-manifest adoption + tm install --reset-agents ([#2505](https://github.com/bobmatnyc/trusty-tools/pull/2505)) ([`2d78f8e`](https://github.com/bobmatnyc/trusty-tools/commit/2d78f8e6ff251ff8d0f1bcf88b419410c0d65b0c))
- bundled agent guidance — foreground execution + PM-directive scoping (closes #2501, #2502) ([#2503](https://github.com/bobmatnyc/trusty-tools/pull/2503)) ([`d37aaf9`](https://github.com/bobmatnyc/trusty-tools/commit/d37aaf9300e52bfba262a0d684d6b8811f33b522))
- uniform TRUSTY_MPM_URL resolution across tm CLI client construction ([#2499](https://github.com/bobmatnyc/trusty-tools/pull/2499)) ([`8a985df`](https://github.com/bobmatnyc/trusty-tools/commit/8a985dff46117465bd38f574580095aea8978a20))
- drop decommissioned sessions from TUI Sessions pane ([#2497](https://github.com/bobmatnyc/trusty-tools/pull/2497)) ([`13cbb2b`](https://github.com/bobmatnyc/trusty-tools/commit/13cbb2b68d7a1196dadb76559fc2e6ada0466c26))
- surface server error bodies in DaemonClient PATCH/mutation errors ([#2496](https://github.com/bobmatnyc/trusty-tools/pull/2496)) ([`9ae5388`](https://github.com/bobmatnyc/trusty-tools/commit/9ae5388cc3d4356b76bdaf243193fb845502f368))
- launchd-aware bridge no-spawn + /health supervised flag (closes #2486) ([#2491](https://github.com/bobmatnyc/trusty-tools/pull/2491)) ([`e993c18`](https://github.com/bobmatnyc/trusty-tools/commit/e993c18ace1fe9a86f4b5315be7887ed767da710))
- target stored pane_id on resume/restart respawn ([#2467](https://github.com/bobmatnyc/trusty-tools/pull/2467)) ([`10fd418`](https://github.com/bobmatnyc/trusty-tools/commit/10fd4187309dee58a2cef689704eb38ff4b95ed0))
- reconcile stale-Active session before refusing in-place relaunch ([#2456](https://github.com/bobmatnyc/trusty-tools/pull/2456)) ([`16d4365`](https://github.com/bobmatnyc/trusty-tools/commit/16d4365dd963fa26b9fb2799714993e7b01fdf3b))
- SessionEnd hook uses non-destructive pane-preserving stop ([#2455](https://github.com/bobmatnyc/trusty-tools/pull/2455)) ([`d8c70a9`](https://github.com/bobmatnyc/trusty-tools/commit/d8c70a9449dfdc1c6315cb7c7cdd344e3939bba2))
- force_new opt-out so the picker's "launch new session" never adopts a live session (closes #2450) ([#2451](https://github.com/bobmatnyc/trusty-tools/pull/2451)) ([`b07b1ba`](https://github.com/bobmatnyc/trusty-tools/commit/b07b1babeca5701769ff5908ebfc6cbd39ca2801))
- SM context-engine round loss under concurrency + graceful no-memory degradation ([#2360](https://github.com/bobmatnyc/trusty-tools/pull/2360)) ([`ccc028a`](https://github.com/bobmatnyc/trusty-tools/commit/ccc028a9b7b8ae571bc05842b09e0375fbec0f3f))
- inject --task into spawned session pane (turnkey execution) ([#2361](https://github.com/bobmatnyc/trusty-tools/pull/2361)) ([`f95f044`](https://github.com/bobmatnyc/trusty-tools/commit/f95f04423d4fa46664621ad7d39dadf5161bf504))
- dedup stale duplicate session records per project in reconcile_on_boot (closes #2306) ([#2338](https://github.com/bobmatnyc/trusty-tools/pull/2338)) ([`a8b040c`](https://github.com/bobmatnyc/trusty-tools/commit/a8b040cdd1e5e4cf7c68d68a4475f05fde92af03))
- strip leading -- separator from tm mcp args at write and read (closes #2326) ([#2329](https://github.com/bobmatnyc/trusty-tools/pull/2329)) ([`df981bf`](https://github.com/bobmatnyc/trusty-tools/commit/df981bf29a9e2fcbe0f82560e784e872c8a7f8f2))
- strip stale-hash mpm hook entries so managed hook-merge is idempotent (closes #2235) ([#2301](https://github.com/bobmatnyc/trusty-tools/pull/2301)) ([`c2ba862`](https://github.com/bobmatnyc/trusty-tools/commit/c2ba862b65dc32511054312472da8a1e87d469c8))
- resolve clippy 1.97.0 lint regressions blocking CI ([#2284](https://github.com/bobmatnyc/trusty-tools/pull/2284)) ([`8b50ac2`](https://github.com/bobmatnyc/trusty-tools/commit/8b50ac25837d5d95244fe94ed913bc3093a2ce86))

### Changed

- mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))
- migrate root CLAUDE.md to .trusty-mpm/INSTRUCTIONS.md ([#2300](https://github.com/bobmatnyc/trusty-tools/pull/2300)) ([`d484967`](https://github.com/bobmatnyc/trusty-tools/commit/d484967e50c6585f02dee2893ab325ac59cc71ee))

### Documentation

- add trusty-mpm package metadata + repoint trusty-search CI badge to monorepo ([#2292](https://github.com/bobmatnyc/trusty-tools/pull/2292)) ([`cba43a5`](https://github.com/bobmatnyc/trusty-tools/commit/cba43a5698c03ea611f731b6a5bef0809547a93f))


### Fixed

- probe live tmux/process for session liveness, not the stored state field ([#2275](https://github.com/bobmatnyc/trusty-tools/pull/2275)) ([`b5f8b6b`](https://github.com/bobmatnyc/trusty-tools/commit/b5f8b6b7b941b5f8939918f592374e0b4bd76d28))

### Added

- capture tmux window id at session pause and reattach on resume ([#2269](https://github.com/bobmatnyc/trusty-tools/pull/2269)) ([`1959738`](https://github.com/bobmatnyc/trusty-tools/commit/1959738450d9a1b7adaf707d9d5980d6365fe4a7))

### Fixed

- correct banner two_panel right-column no-inner-border rendering ([#2258](https://github.com/bobmatnyc/trusty-tools/pull/2258)) ([`6b9ae4a`](https://github.com/bobmatnyc/trusty-tools/commit/6b9ae4ab17ce0a07b2f69bfb14d3b477c8db6fbe))
- keep resumed panes rooted at the workspace, not $HOME ([#2254](https://github.com/bobmatnyc/trusty-tools/pull/2254)) ([`d2c1edf`](https://github.com/bobmatnyc/trusty-tools/commit/d2c1edf4431b0cba3e94f558fc13fc9c40786307))

### Added

- opt-in deny-by-default pm_guard + warn-only carrier self-check ([#2237](https://github.com/bobmatnyc/trusty-tools/pull/2237)) ([`c3328b6`](https://github.com/bobmatnyc/trusty-tools/commit/c3328b62ffc8f26a5e476d2d18c39a931cfb8a11))

### Fixed

- delegation persona reaches resume path + harden tm connect ([#2239](https://github.com/bobmatnyc/trusty-tools/pull/2239)) ([`df09b16`](https://github.com/bobmatnyc/trusty-tools/commit/df09b1655e9b316396ecbda47cd29395a057cf18))
- resolve stable binary for hooks/statusLine + self-heal stale paths (closes #2229) ([#2234](https://github.com/bobmatnyc/trusty-tools/pull/2234)) ([`ae87bbb`](https://github.com/bobmatnyc/trusty-tools/commit/ae87bbb3827b9dfc9fccae4732c66104af6912b7))

### Changed

- re-cut as 0.19.1: depends on trusty-common 0.22.2 (0.19.0 was git-tagged but
  never published to crates.io, so republishing the same content under a new
  patch version rather than moving an existing tag). Ships #2214/#2229/#2230/#2231.

### Added

- seed outputStyle/statusLine into tm-owned config dir ([#2215](https://github.com/bobmatnyc/trusty-tools/pull/2215)) ([`2ebb6e1`](https://github.com/bobmatnyc/trusty-tools/commit/2ebb6e19993524aae4df038041c4c1d1cdc64dd0))

### Added

- statusline shows session + weekly account-usage % ([#2141](https://github.com/bobmatnyc/trusty-tools/pull/2141)) ([`18a7942`](https://github.com/bobmatnyc/trusty-tools/commit/18a79426bd9b618910fbc3869d1929f98de19aec))
- statusline renders context >50% in red ([#2099](https://github.com/bobmatnyc/trusty-tools/pull/2099)) ([`a821b9d`](https://github.com/bobmatnyc/trusty-tools/commit/a821b9da89d7c076dfb4557094f632859b53642e))
- manage trusty-search index lifecycle for session worktrees ([#2094](https://github.com/bobmatnyc/trusty-tools/pull/2094)) ([`299e993`](https://github.com/bobmatnyc/trusty-tools/commit/299e9931edd2b7faf9236bd5ccc3b6ddb038329d))
- project config stores preferred gh user; default gh ops to it ([#2087](https://github.com/bobmatnyc/trusty-tools/pull/2087)) ([`d49e385`](https://github.com/bobmatnyc/trusty-tools/commit/d49e385826c0befb0d4cece01a032c71e67e1aa3))
- name managed-session worktrees by tmux session name, not UUID ([#2076](https://github.com/bobmatnyc/trusty-tools/pull/2076)) ([`39abd2e`](https://github.com/bobmatnyc/trusty-tools/commit/39abd2e02b47e5b4e739ad30de57c33cfdf4dc54))

### Fixed

- guarantee PM delegation persona loads (CLAUDE.md carrier + project-tier output-style + daemon-adapter system-prompt injection) ([#2129](https://github.com/bobmatnyc/trusty-tools/pull/2129)) ([`3a071a0`](https://github.com/bobmatnyc/trusty-tools/commit/3a071a0a2897911279433522d2564cb532f310be))
- pm_guard exempts native delegated sub-agent edits; retire global UNRESTRICTED bypass ([#2107](https://github.com/bobmatnyc/trusty-tools/pull/2107)) ([`0f413d0`](https://github.com/bobmatnyc/trusty-tools/commit/0f413d0c5d611178dbd5344475ae61ba5fa21213))
- PM summarizes orchestration in prose instead of raw pm_summary JSON ([#2045](https://github.com/bobmatnyc/trusty-tools/pull/2045)) ([`53731f4`](https://github.com/bobmatnyc/trusty-tools/commit/53731f4849436607cdef55237680a230c798d94f))
- statusline shows tmux session name instead of worktree UUID branch ([#2035](https://github.com/bobmatnyc/trusty-tools/pull/2035)) ([`bbafad1`](https://github.com/bobmatnyc/trusty-tools/commit/bbafad11063accc2631be4fd97cb610c13152faa))
- reach trusty-memory over discovered JSON-RPC, never a hardcoded port ([#2040](https://github.com/bobmatnyc/trusty-tools/pull/2040)) ([`e0f41c5`](https://github.com/bobmatnyc/trusty-tools/commit/e0f41c51f1baa7ddf0e427cb5c7e86cbe9bba5fa))

### Changed

- bump version 0.16.0 -> 0.16.1 ([#2139](https://github.com/bobmatnyc/trusty-tools/pull/2139)) ([`931917c`](https://github.com/bobmatnyc/trusty-tools/commit/931917cdc2606f563be7c711a5adb2842e7ce229))

### Documentation

- bundled architecture doc for memory-MCP, session/worktree, search-index ([#2096](https://github.com/bobmatnyc/trusty-tools/pull/2096)) ([`d82e15a`](https://github.com/bobmatnyc/trusty-tools/commit/d82e15ae9367a4c17cdd8fb714e053eb6da9cab7))

### Added


### Fixed


### Documentation


### Added

- in-place relaunch current session + on-exit hint (#2023 C+D) ([#2027](https://github.com/bobmatnyc/trusty-tools/pull/2027)) ([`dba2195`](https://github.com/bobmatnyc/trusty-tools/commit/dba21950daedb48e909ffa2aefd9c12b0d47537a))
- export TM_MANAGED_SESSION_ID into managed pane shell ([#2025](https://github.com/bobmatnyc/trusty-tools/pull/2025)) ([`879343b`](https://github.com/bobmatnyc/trusty-tools/commit/879343b93c720c69b6c706535de7d2d8e0db8021))
- non-destructive stop for runtime-exited sessions (#2023 A) ([#2024](https://github.com/bobmatnyc/trusty-tools/pull/2024)) ([`bca3685`](https://github.com/bobmatnyc/trusty-tools/commit/bca3685d665e67bcc0e1ba2f862410521b22c6fd))
- add tm session delete <id> [--force] ([#2021](https://github.com/bobmatnyc/trusty-tools/pull/2021)) ([`72cf975`](https://github.com/bobmatnyc/trusty-tools/commit/72cf9754cf507bf5ee5271e01412a9bb41b1147a))
- reformat tm statusline ([#2018](https://github.com/bobmatnyc/trusty-tools/pull/2018)) ([`138b0fb`](https://github.com/bobmatnyc/trusty-tools/commit/138b0fb8b76bf5fe22f56ddb74435fe1db7202ed))
- managed sessions launch with tm-owned CLAUDE_CONFIG_DIR + full roster (DOC-34, #1996) ([#2002](https://github.com/bobmatnyc/trusty-tools/pull/2002)) ([`7c2d13d`](https://github.com/bobmatnyc/trusty-tools/commit/7c2d13dc7c1d39e747b65a9e6b23a7be45ec4fdc))
- add /tm-init project-initialization skill ([#1997](https://github.com/bobmatnyc/trusty-tools/pull/1997)) ([`4c7332b`](https://github.com/bobmatnyc/trusty-tools/commit/4c7332b1abb8ab13ca8c9700625382823f57f7af))
- native gh-account awareness — statusline @login + doctor check ([#1994](https://github.com/bobmatnyc/trusty-tools/pull/1994)) ([`521f696`](https://github.com/bobmatnyc/trusty-tools/commit/521f696f037092cd60493dfc748eeea5749ec867))
- extract managed-session naming to trusty-common; trusty-agents adopts it (DOC-33 Phase 1, SPEC-ONESM-01) ([#1989](https://github.com/bobmatnyc/trusty-tools/pull/1989)) ([`025941d`](https://github.com/bobmatnyc/trusty-tools/commit/025941dfe68b7806f1f7f6b82bc06923d1cd5b9e))
- add /tm-session-pause and /tm-session-resume skills ([#1986](https://github.com/bobmatnyc/trusty-tools/pull/1986)) ([`0829711`](https://github.com/bobmatnyc/trusty-tools/commit/0829711037723085f701363b93c2de67214425e7))
- enforce PM delegation prohibitions via PreToolUse guard ([#1985](https://github.com/bobmatnyc/trusty-tools/pull/1985)) ([`ea14e69`](https://github.com/bobmatnyc/trusty-tools/commit/ea14e6981e40b1dac5d7faaf3d26476d0351932d))

### Fixed

- route Claude spawn through shared spawn_command builder ([#2017](https://github.com/bobmatnyc/trusty-tools/pull/2017)) ([`96b1ff5`](https://github.com/bobmatnyc/trusty-tools/commit/96b1ff5ef030e2cbafbca7fac9c3bcc5de746ea8))
- stop managed-hook duplicate accumulation in shared settings.json ([#2019](https://github.com/bobmatnyc/trusty-tools/pull/2019)) ([`7706bf7`](https://github.com/bobmatnyc/trusty-tools/commit/7706bf72f17afabc9df306b884a993faa31da3e4))
- resilient resume — existence-check before --resume ([#2016](https://github.com/bobmatnyc/trusty-tools/pull/2016)) ([`85197db`](https://github.com/bobmatnyc/trusty-tools/commit/85197db401499139d3a71069eb122ae4685fdcf4))
- correct env arg order in managed spawn (-u before CLAUDE_CONFIG_DIR) ([#2009](https://github.com/bobmatnyc/trusty-tools/pull/2009)) ([`13a949d`](https://github.com/bobmatnyc/trusty-tools/commit/13a949da42f4d12bf3e535bb638d4ee77d030820))
- make bundled PM skill examples stack-neutral (closes #2005) ([#2006](https://github.com/bobmatnyc/trusty-tools/pull/2006)) ([`53c3cdc`](https://github.com/bobmatnyc/trusty-tools/commit/53c3cdce3b99e2635565a58a1e722e3abcffc405))
- auto-reconcile zombie sessions on guided resume instead of erroring (closes #2001) ([#2004](https://github.com/bobmatnyc/trusty-tools/pull/2004)) ([`f635216`](https://github.com/bobmatnyc/trusty-tools/commit/f635216c4b1e6edf7463b2202283c2b39e0e180e))
- statusline shows user/repo, drops duplicate session-id branch ([#1991](https://github.com/bobmatnyc/trusty-tools/pull/1991)) ([`58f5b1f`](https://github.com/bobmatnyc/trusty-tools/commit/58f5b1f9df62dfae183389f5e279d8579f07de13))
- graceful CLI session termination + managed-session delegation gating ([#1979](https://github.com/bobmatnyc/trusty-tools/pull/1979)) ([`139bde2`](https://github.com/bobmatnyc/trusty-tools/commit/139bde2938f8edf33cd71df1d0a2b2a09ef1580c))

### Added

- compact splash art update, supersede giant-robot banner ([#1973](https://github.com/bobmatnyc/trusty-tools/pull/1973)) ([`9dd65fb`](https://github.com/bobmatnyc/trusty-tools/commit/9dd65fb875585cb41aa6a6affe7aae9a01df578a))
- switch session naming to tm-<leaf>-NN pattern ([#1966](https://github.com/bobmatnyc/trusty-tools/pull/1966)) ([`5c20a3d`](https://github.com/bobmatnyc/trusty-tools/commit/5c20a3dc3a0be4ea93ec298d7ee04ecabff0fb46))
- giant-robot «TRUSTY» banner art + stale banner.txt seed refresh ([#1933](https://github.com/bobmatnyc/trusty-tools/pull/1933)) ([`25cd224`](https://github.com/bobmatnyc/trusty-tools/commit/25cd22434d2bce527c6b0b4d7cb123c73d167928))
- extend per-stage provisioning progress to in-project and local-path spawns (closes #1919) ([#1923](https://github.com/bobmatnyc/trusty-tools/pull/1923)) ([`211b7ba`](https://github.com/bobmatnyc/trusty-tools/commit/211b7ba211873b9505090b8ee84ad521ebf97f19))
- unify session start with protected-path routing, rename sessions->session, bare tm shortcut (closes #1916) ([#1920](https://github.com/bobmatnyc/trusty-tools/pull/1920)) ([`0f40c01`](https://github.com/bobmatnyc/trusty-tools/commit/0f40c01085d15d6ec5f7f2424593640ad11da23e))
- add Trusty MPM v0.12.0 wordmark to splash banner (closes #1907) ([#1912](https://github.com/bobmatnyc/trusty-tools/pull/1912)) ([`d02d75c`](https://github.com/bobmatnyc/trusty-tools/commit/d02d75c26fd7d134c29a4f90d7434a2c094039ef))
- add tm meta-harness self-awareness text to BASE_PM identity (closes #1906) ([#1911](https://github.com/bobmatnyc/trusty-tools/pull/1911)) ([`076dfd3`](https://github.com/bobmatnyc/trusty-tools/commit/076dfd318740404a650cae4d52d613995a4c4459))
- complete mpm-* to tm-* skill fork, fork-safe one-time cleanup migration ([#1910](https://github.com/bobmatnyc/trusty-tools/pull/1910)) ([`0d3d93b`](https://github.com/bobmatnyc/trusty-tools/commit/0d3d93b68939ab672088affb0744070e945032da))
- stream real per-stage provisioning progress + trigger index reindex on session launch ([#1909](https://github.com/bobmatnyc/trusty-tools/pull/1909)) ([`287a49e`](https://github.com/bobmatnyc/trusty-tools/commit/287a49eeb896d56e51334fdabc02ecfc6b885e10))
- add report-only dry-run gate to idle auto-teardown (closes #1783) ([#1899](https://github.com/bobmatnyc/trusty-tools/pull/1899)) ([`3a7adf3`](https://github.com/bobmatnyc/trusty-tools/commit/3a7adf33c9512893881b8ba4c7e5ca54e6661f2a))
- rebuild PM skills as /tm- portfolio + fix skill-deploy bugs ([#1872](https://github.com/bobmatnyc/trusty-tools/pull/1872)) ([`2a245e8`](https://github.com/bobmatnyc/trusty-tools/commit/2a245e8724959ef7f38cb95e59c0367e36932cf8))
- auto-seed R3 identity prompt-fact at provision (DOC-28 Phase 2, epic #1855) ([`1b553c1`](https://github.com/bobmatnyc/trusty-tools/commit/1b553c17d5a51fcd324dd538ce5ed42cda3bdb97))

### Fixed

- clarify tool-output compression scope, ZTK/RTK status (closes #1944) ([#1952](https://github.com/bobmatnyc/trusty-tools/pull/1952)) ([`7b0a5bd`](https://github.com/bobmatnyc/trusty-tools/commit/7b0a5bd1f839ac531ce01e45db0bef9166aa5739))
- correct stale PM instruction template content (closes #1943) ([#1951](https://github.com/bobmatnyc/trusty-tools/pull/1951)) ([`c735452`](https://github.com/bobmatnyc/trusty-tools/commit/c735452c6b21df7ff24ee8a5925d131da8feb8cc))
- register provisioned native sessions so session_list finds them (closes #1946) ([#1949](https://github.com/bobmatnyc/trusty-tools/pull/1949)) ([`b8ff3cc`](https://github.com/bobmatnyc/trusty-tools/commit/b8ff3cc5d280378f0853ac3634b358291e613124))
- reconcile manifest drift, scope roster by project language, fix catalog sync (closes #1940, closes #1941, closes #1947) ([#1950](https://github.com/bobmatnyc/trusty-tools/pull/1950)) ([`e136d6c`](https://github.com/bobmatnyc/trusty-tools/commit/e136d6cd0b141071200b89b244a38ca6549b2cf9))
- clarify agent_delegate is a tracking gate, not the delegation path (closes #1942) ([#1948](https://github.com/bobmatnyc/trusty-tools/pull/1948)) ([`d38359f`](https://github.com/bobmatnyc/trusty-tools/commit/d38359fe29c40302c1482c8a1e3949c42e705154))
- palace-level alias resolution for claude-mpm parity (owner-repo -> bare palace) ([#1945](https://github.com/bobmatnyc/trusty-tools/pull/1945)) ([`af7f904`](https://github.com/bobmatnyc/trusty-tools/commit/af7f90499402971ac65aed5b104cde251e182599))
- address review follow-ups from #1936 (closes #1937) ([#1938](https://github.com/bobmatnyc/trusty-tools/pull/1938)) ([`1c0afa3`](https://github.com/bobmatnyc/trusty-tools/commit/1c0afa3c985680f671919811991c7eb5ef51d21c))
- use shared base checkout + git worktree per session (closes #1935) ([#1936](https://github.com/bobmatnyc/trusty-tools/pull/1936)) ([`40b9e3e`](https://github.com/bobmatnyc/trusty-tools/commit/40b9e3e2bfdef0d31ae432b6dbe43e885422a13a))
- daemon managed-spawn deploys agents/skills to worktree, not real ~/.claude (closes #1931) ([#1934](https://github.com/bobmatnyc/trusty-tools/pull/1934)) ([`d0c655d`](https://github.com/bobmatnyc/trusty-tools/commit/d0c655d142c8f2e043d09546c5905e7d165c4399))
- standalone load_alias deploys to isolated config, not real ~/.claude (closes #1927) ([#1930](https://github.com/bobmatnyc/trusty-tools/pull/1930)) ([`81e1924`](https://github.com/bobmatnyc/trusty-tools/commit/81e1924f662f1cee5b013d2db3c3840ee480bb98))
- resolve tm statusline to absolute path, heal stale bare entries (closes #1914) ([#1925](https://github.com/bobmatnyc/trusty-tools/pull/1925)) ([`104b4bf`](https://github.com/bobmatnyc/trusty-tools/commit/104b4bfb649315594cbad7a36931bf136b6a08e3))
- auto-refresh stale skill source dir, log skill deployment counts (closes #1917) ([#1922](https://github.com/bobmatnyc/trusty-tools/pull/1922)) ([`a124a2e`](https://github.com/bobmatnyc/trusty-tools/commit/a124a2e659a1eb620b2bdaeba36d492c099dc5cf))
- prevent orphan-GC from killing legitimate sessions across daemon restart (closes #1918) ([#1921](https://github.com/bobmatnyc/trusty-tools/pull/1921)) ([`1c90c11`](https://github.com/bobmatnyc/trusty-tools/commit/1c90c11aff433c1bdfc0ccc0bf94a14050858cdc))
- call prepare_session in spawn_managed_inproject, add statusline self-heal on resume (closes #1913) ([#1915](https://github.com/bobmatnyc/trusty-tools/pull/1915)) ([`7c3b052`](https://github.com/bobmatnyc/trusty-tools/commit/7c3b05222deaa06146b16b8ca7b0d0a9fab1c3f9))
- correct launchd plist detection in guided autostart ([#1901](https://github.com/bobmatnyc/trusty-tools/pull/1901)) ([`aa5da12`](https://github.com/bobmatnyc/trusty-tools/commit/aa5da12ce413b8a186d6afeb251ca48b758fd70e))
- auto-transition sessions to stopped when runtime process exits (closes #1814) ([#1894](https://github.com/bobmatnyc/trusty-tools/pull/1894)) ([`b4bee16`](https://github.com/bobmatnyc/trusty-tools/commit/b4bee163541f444b58de396f3637faae36db65e1))
- clean up managed-clone worktree dirs on decommission + reap orphans (closes #1838) ([#1895](https://github.com/bobmatnyc/trusty-tools/pull/1895)) ([`8843f9c`](https://github.com/bobmatnyc/trusty-tools/commit/8843f9c1ef2e2421cc06b32cb6c233ea9e37988b))
- unify workspace-root resolution + handle pre-existing old-layout dirs (closes #1805, closes #1807) ([#1896](https://github.com/bobmatnyc/trusty-tools/pull/1896)) ([`875d8e3`](https://github.com/bobmatnyc/trusty-tools/commit/875d8e310a2c0f0487b1bc0cd9e2e33b8e1ddc00))
- require opt-in before MCP-triggered session spawn for unregistered repos (#1836, #1837) ([`2a6252f`](https://github.com/bobmatnyc/trusty-tools/commit/2a6252fc06552e08976cf813f0b2de1af64a1e56))
- session attach uses switch-client inside tmux (nested-tmux fix) ([#1875](https://github.com/bobmatnyc/trusty-tools/pull/1875)) ([`93324c2`](https://github.com/bobmatnyc/trusty-tools/commit/93324c201ce9c18d6aa5a634da9361580c787e91))
- output-style resolution + test-isolation hygiene (#1860, #1863, #1858) ([`8a9a4f9`](https://github.com/bobmatnyc/trusty-tools/commit/8a9a4f9db79025d59fc1263e023984492d0af3e1))

### Changed

- hoist compress::tool_output from trusty-agents ([#1959](https://github.com/bobmatnyc/trusty-tools/pull/1959)) ([#1968](https://github.com/bobmatnyc/trusty-tools/pull/1968)) ([`7cf93b9`](https://github.com/bobmatnyc/trusty-tools/commit/7cf93b9ab3918aff316238bdfe540a4053aa971d))
- split inproject.rs to satisfy 500-SLOC production cap ([#1898](https://github.com/bobmatnyc/trusty-tools/pull/1898)) ([`43066c2`](https://github.com/bobmatnyc/trusty-tools/commit/43066c2a30cd9bdc6513554caff9ad2da54defd2))

### Changed: CLI command group `session` → `sessions` (issue #1394)

The top-level CLI command group was renamed from the singular `session` to the
plural **`sessions`** to match the `/api/v1/sessions/*` HTTP API surface. Every
subcommand is now invoked under the plural name, e.g. `tm sessions tui`,
`tm sessions ls`, `tm sessions new`.

- The singular `session` spelling is **removed entirely** — it is not retained
  as an alias. Invoking `tm session …` now fails with an
  unrecognized-subcommand error. Update any scripts or muscle memory to
  `tm sessions …`.
- This is a CLI-only change; the HTTP API (already `/api/v1/sessions/*`) and the
  separate `session-manager` / `sm` coordinator command are unaffected.

### Deprecated: verbose managed session-lifecycle verbs (issue #1205)

The managed session-lifecycle CLI verbs were renamed to the cleaner, symmetric
`stop` / `resume` / `decommission` family. The old verbose verbs still work but
now emit a one-line deprecation notice to **stderr** on every invocation and
will be removed in a future release.

| Deprecated verb | Use instead | Behavior |
|-----------------|-------------|----------|
| `tm sessions runtime-stop <id>` | `tm sessions stop <id>` | Stop the runtime, keep the workspace (resumable) |
| `tm sessions managed-stop <id>` | `tm sessions stop <id>` | Same as `runtime-stop` |
| `tm sessions managed-resume <id>` | `tm sessions resume <id>` | Re-spawn the runtime in the existing workspace |

- The deprecated verbs are hidden from `tm sessions --help` but continue to parse
  for backward compatibility.
- Each deprecated invocation prints `warning: '<old>' is deprecated; use '<new>'`
  to stderr; stdout stays clean for scripts.
- `tm sessions decommission <id>` (terminal teardown: remove workspace from disk)
  is unchanged.

## [0.19.4] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [0.14.0] — 2026-07-01

### Added

DOC-28 self-awareness (R1-R4 of `docs/specs/trusty-mpm-self-awareness.md`), closing the
gaps behind the "self-awareness incident" where a session conflated this Rust
`trusty-mpm` with the unrelated Python `claude-mpm`, and no mechanism detected
that its instructions never loaded ([#1855](https://github.com/bobmatnyc/trusty-tools/pull/1855)) ([#1859](https://github.com/bobmatnyc/trusty-tools/pull/1859)) ([`5708d95`](https://github.com/bobmatnyc/trusty-tools/commit/5708d95310dd02c903502d812990c8c6d9d743e8)):

- **R1 — canonical self-description doc**: new bundled
  `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`, deployed via
  `bundle_all.rs::ALL` with `InstallPolicy::Overwrite` so `tm install` installs
  it to `~/.trusty-mpm/framework/docs/`.
- **R2 — identity protocol in instructions**: "Identity & Self-Awareness
  Protocol (Non-Overridable)" section added verbatim to `BASE_SM.md`'s
  non-overridable floor and to all three bundled output styles
  (`trusty-mpm.md`, `trusty-mpm-teacher.md`, `trusty-mpm-research.md`),
  routing identity questions through memory + the R1 doc and forbidding
  shell-probing (`pip3 show`, `which claude-mpm`).
- **R3 — manual identity-fact seeding**: documents the `kg_assert` identity-fact
  seed step in the R1 doc (automatic seeding from `prepare_session` is deferred
  future work per spec §7 Phase 2/§10).
- **R4 — `tm doctor` output-style check + load marker**: new
  `daemon/doctor_output_style.rs` probe validates that the effective
  `outputStyle` setting resolves to a real bundled style file (Ok/Warn/Fail per
  spec, reproducing the exact incident condition as a Fail), plus the
  greppable `<!-- trusty-mpm-instructions-loaded: v1 -->` load marker at the
  top of each floor section.

## [0.13.0] — 2026-06-30

### Added

- route tm CLI through console gateway with direct fallback ([#1852](https://github.com/bobmatnyc/trusty-tools/pull/1852)) ([`b1b58bc`](https://github.com/bobmatnyc/trusty-tools/commit/b1b58bc30a423db98d21d5254c3d4d8ac71ab8a5))
- wire trusty-mpm into console reverse proxy ([#1850](https://github.com/bobmatnyc/trusty-tools/pull/1850)) ([`970d297`](https://github.com/bobmatnyc/trusty-tools/commit/970d297bf9448cf74b3117445401524bd17b20e4))
- idle auto-suspend + scrollback snapshot + resume restoration (opt-in) ([#1816](https://github.com/bobmatnyc/trusty-tools/pull/1816)) ([#1822](https://github.com/bobmatnyc/trusty-tools/pull/1822)) ([`5e28313`](https://github.com/bobmatnyc/trusty-tools/commit/5e283135fa0ea959aed720573a1302c822c02fb9))
- runtime-editable splash art + block-robot default banner ([#1825](https://github.com/bobmatnyc/trusty-tools/pull/1825)) ([#1829](https://github.com/bobmatnyc/trusty-tools/pull/1829)) ([`bd4a72a`](https://github.com/bobmatnyc/trusty-tools/commit/bd4a72a9d1dce02d27db14b894dbc725394247d5))
- auto-register git project alias on tm launch + tm ls shows local paths ([#1819](https://github.com/bobmatnyc/trusty-tools/pull/1819)) ([`436f7c9`](https://github.com/bobmatnyc/trusty-tools/commit/436f7c9194077822fb0d44de9cff4fd1f4862909))
- kawaii row-of-three pixel-bots replace scary ASCII art ([#1811](https://github.com/bobmatnyc/trusty-tools/pull/1811)) ([#1812](https://github.com/bobmatnyc/trusty-tools/pull/1812)) ([`d8e2111`](https://github.com/bobmatnyc/trusty-tools/commit/d8e2111d882c7237c77214dd122d6337d6d8fde3))
- unify daily tm banner with tm banner + hide decommissioned tombstones (#1808, #1809) ([#1810](https://github.com/bobmatnyc/trusty-tools/pull/1810)) ([`fba0160`](https://github.com/bobmatnyc/trusty-tools/commit/fba0160a2d12132ef36c2c069502b7acd9be3a5e))
- unify managed clone on shared base + per-session .worktrees/ ([#1803](https://github.com/bobmatnyc/trusty-tools/pull/1803)) ([#1804](https://github.com/bobmatnyc/trusty-tools/pull/1804)) ([`c32fc0c`](https://github.com/bobmatnyc/trusty-tools/commit/c32fc0c0623a16757c9d0a4d65d8150a29e6c2d0))
- refine tm robot banner — clip art, dedupe version, owner/repo + managed path ([#1794](https://github.com/bobmatnyc/trusty-tools/pull/1794)) ([`9183e11`](https://github.com/bobmatnyc/trusty-tools/commit/9183e11d52ee8a1076ed09c894c746af1e493ab7))
- redirect tm launch + spawn_managed_local to managed clone ([#1590](https://github.com/bobmatnyc/trusty-tools/pull/1590)) ([#1796](https://github.com/bobmatnyc/trusty-tools/pull/1796)) ([`6c0af3c`](https://github.com/bobmatnyc/trusty-tools/commit/6c0af3c798686a0eb62cdb734b0febc39d0d265c))
- detach returns to tm picker + daemon/clone cwd hardening ([#1795](https://github.com/bobmatnyc/trusty-tools/pull/1795)) ([`3b0e723`](https://github.com/bobmatnyc/trusty-tools/commit/3b0e7231e85ca8fbc53dbd55bb4968d4d96e811c))
- include repo name in managed tmux session names ([#1789](https://github.com/bobmatnyc/trusty-tools/pull/1789)) ([#1791](https://github.com/bobmatnyc/trusty-tools/pull/1791)) ([`c1887de`](https://github.com/bobmatnyc/trusty-tools/commit/c1887defa898e32ee4a424869a810b40567aa979))
- session-manager daily QoL fixes — ls --source-id, info fallback, honest decommission ([#1787](https://github.com/bobmatnyc/trusty-tools/pull/1787)) ([#1788](https://github.com/bobmatnyc/trusty-tools/pull/1788)) ([`9e9c795`](https://github.com/bobmatnyc/trusty-tools/commit/9e9c795ed07399a6e252e2071d8bc0c161dba1ff))
- add context compaction efficiency segment ([#1774](https://github.com/bobmatnyc/trusty-tools/pull/1774)) ([`0594d8f`](https://github.com/bobmatnyc/trusty-tools/commit/0594d8f10e68d485ee190f7b76f072709eb18158))
- tm guided-default auto-start daemon + github-* SSH alias support ([#1775](https://github.com/bobmatnyc/trusty-tools/pull/1775)) ([#1776](https://github.com/bobmatnyc/trusty-tools/pull/1776)) ([`20374a2`](https://github.com/bobmatnyc/trusty-tools/commit/20374a26e1b445f11dfabe6a46ce69325decd5f8))
- DOC-28 cutover catch-up runtime — watermark + git/palace + auto-inject (PR2/3/4, #1762) ([`e7e23ea`](https://github.com/bobmatnyc/trusty-tools/commit/e7e23ea2ae1a679e285391ea452272ec5bbbfee2))
- DOC-28 cutover bridge core — tm sessions catchup (PR1, #1762) ([`d66a989`](https://github.com/bobmatnyc/trusty-tools/commit/d66a989a2f6cd4e208fd1d69092d7f644da3e23e))

### Fixed

- session-worktree prune/decommission hardening ([#1845](https://github.com/bobmatnyc/trusty-tools/pull/1845)) ([#1853](https://github.com/bobmatnyc/trusty-tools/pull/1853)) ([`ff970ed`](https://github.com/bobmatnyc/trusty-tools/commit/ff970ed88549f2536f47c00bc232e70dde2561bb))
- normalise lock-file URL before TCP probe in banner ([#1847](https://github.com/bobmatnyc/trusty-tools/pull/1847)) ([#1848](https://github.com/bobmatnyc/trusty-tools/pull/1848)) ([`df5c330`](https://github.com/bobmatnyc/trusty-tools/commit/df5c330d14fd43bfd0f8174582ef52a74adf0b7c))
- CLI ergonomics fixes for tm sessions ([#1846](https://github.com/bobmatnyc/trusty-tools/pull/1846)) ([`9e4f4b9`](https://github.com/bobmatnyc/trusty-tools/commit/9e4f4b98f6e236ec6fa106f713258fba5a03ba2a))
- managed-session lifecycle correctness ([#1840](https://github.com/bobmatnyc/trusty-tools/pull/1840)) ([#1844](https://github.com/bobmatnyc/trusty-tools/pull/1844)) ([`78f2bc2`](https://github.com/bobmatnyc/trusty-tools/commit/78f2bc29a9ddb14a2bd7b9c23e1b36e740ee36f3))
- banner/health/entry UX fixes ([#1839](https://github.com/bobmatnyc/trusty-tools/pull/1839)) ([#1843](https://github.com/bobmatnyc/trusty-tools/pull/1843)) ([`1863c22`](https://github.com/bobmatnyc/trusty-tools/commit/1863c22692b85621c3e15ef30aa7436b5b0883da))
- skip worktree checkouts in auto-registration + document TRUSTY_MPM_BANNER_FILE ([#1835](https://github.com/bobmatnyc/trusty-tools/pull/1835)) ([`d2e1ab8`](https://github.com/bobmatnyc/trusty-tools/commit/d2e1ab84a8f0a37dac6e14e7d03cbc02b566b630))
- orphan-GC log-spam reduction + key-match regression tests (closes #1813) ([#1823](https://github.com/bobmatnyc/trusty-tools/pull/1823)) ([`836f393`](https://github.com/bobmatnyc/trusty-tools/commit/836f3934e005a3436d1f775480b511a23017218e))
- RAII guard kills leaked tmux sessions after test/error ([#1815](https://github.com/bobmatnyc/trusty-tools/pull/1815)) ([#1821](https://github.com/bobmatnyc/trusty-tools/pull/1821)) ([`af67403`](https://github.com/bobmatnyc/trusty-tools/commit/af67403f3c5fe016cc03c532faafc70d19e3796e))
- stop session-manager tests leaking real tmux sessions into production store ([#1790](https://github.com/bobmatnyc/trusty-tools/pull/1790)) ([#1793](https://github.com/bobmatnyc/trusty-tools/pull/1793)) ([`b3410e4`](https://github.com/bobmatnyc/trusty-tools/commit/b3410e4fa5373a7df6759a369e3ccc38d99b4a24))
- session-manager on-ramp blockers — source_id backfill + first-run clone feedback ([#1780](https://github.com/bobmatnyc/trusty-tools/pull/1780)) ([#1781](https://github.com/bobmatnyc/trusty-tools/pull/1781)) ([`313b962`](https://github.com/bobmatnyc/trusty-tools/commit/313b962c00e92b036fec76ad09d8ca72256ce367))
- non-GitHub remote refusal no longer blames daemon ([#1777](https://github.com/bobmatnyc/trusty-tools/pull/1777)) ([#1778](https://github.com/bobmatnyc/trusty-tools/pull/1778)) ([`cc9d152`](https://github.com/bobmatnyc/trusty-tools/commit/cc9d152b81991bba15c35553ec95fcfd596213a8))

### Changed

- extract DOC-28 catch-up engine behind catchup feature (PR1, #1762) ([`addfdbb`](https://github.com/bobmatnyc/trusty-tools/commit/addfdbb04ed78028887a0e782afe7cfe83c10b46))

---

## [0.12.0] — 2026-06-27

### Added

- two-panel full-width banner (robot left, info right, natural height) ([#1759](https://github.com/bobmatnyc/trusty-tools/pull/1759)) ([`37f3810`](https://github.com/bobmatnyc/trusty-tools/commit/37f3810e3dd238b4b3509c0b65d48393d355e934))
- full-screen rust robot banner + bypass-permissions launch ([#1755](https://github.com/bobmatnyc/trusty-tools/pull/1755)) ([`0924589`](https://github.com/bobmatnyc/trusty-tools/commit/092458947d3c5487e188ba260744754cfd486f37))
- ungraceful-exit handling + --resume conversation continuity (closes #1744) ([#1748](https://github.com/bobmatnyc/trusty-tools/pull/1748)) ([`40989bd`](https://github.com/bobmatnyc/trusty-tools/commit/40989bd30f2e35f9b365cdb7a877348505f9e8c1))
- expanded pre-launch welcome panel — recent commits, service status, TM commands (closes #1743) ([#1747](https://github.com/bobmatnyc/trusty-tools/pull/1747)) ([`689a9be`](https://github.com/bobmatnyc/trusty-tools/commit/689a9bed62b39640f099f038c453617a3d16d73c))
- tm welcome banner box + rich Claude Code statusline + tmux detach hint ([#1740](https://github.com/bobmatnyc/trusty-tools/pull/1740)) ([`db0a115`](https://github.com/bobmatnyc/trusty-tools/commit/db0a11553da9e81a1ee8f36b4204ee0f768f0a41))
- guided-default session picker when tm run from a repo ([#1705](https://github.com/bobmatnyc/trusty-tools/pull/1705)) ([#1729](https://github.com/bobmatnyc/trusty-tools/pull/1729)) ([`40ec125`](https://github.com/bobmatnyc/trusty-tools/commit/40ec1252d44cb29c3540b69b70bc052a935851e0))
- chat session manager MVP — force flag, turn tools, palace_dream, Task drawer (closes #1719 #1720 #1721 #1722) ([#1723](https://github.com/bobmatnyc/trusty-tools/pull/1723)) ([`7b22f28`](https://github.com/bobmatnyc/trusty-tools/commit/7b22f28e2c4f256eda0678a01fac16bd1584685b))
- in-project protected workspace + claude-mpm parity (epic #1590) ([#1715](https://github.com/bobmatnyc/trusty-tools/pull/1715)) ([`abd9914`](https://github.com/bobmatnyc/trusty-tools/commit/abd991451ba84a771ff91fc06e86e390de30ac32))
- usability sprint 1 — lock-file URL, startup prompts, TASK.md, offline swagger ([#1697](https://github.com/bobmatnyc/trusty-tools/pull/1697)) ([`d5e7e37`](https://github.com/bobmatnyc/trusty-tools/commit/d5e7e3776852d353b407d04d8623376f98298f56))
- WI-5 follow-ups — OpenRouter classifier call + auth-timeout auto-stop (closes #1648, closes #1649) ([#1656](https://github.com/bobmatnyc/trusty-tools/pull/1656)) ([`6c71d64`](https://github.com/bobmatnyc/trusty-tools/commit/6c71d646aba04e3e530b081150166518ec827dd3))
- pin palace slug in standalone MCP injection (closes #1651) ([#1655](https://github.com/bobmatnyc/trusty-tools/pull/1655)) ([`663c9ea`](https://github.com/bobmatnyc/trusty-tools/commit/663c9eab5680830157a864484f01985eaabf0dba))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))
- SESSCTL WI-5 auth + cost model (closes #1596) ([#1647](https://github.com/bobmatnyc/trusty-tools/pull/1647)) ([`c51a5f6`](https://github.com/bobmatnyc/trusty-tools/commit/c51a5f6ae68cee071d320a92b23b168cb7c4e441))

### Fixed

- absolute-path + project-scope + opt-out for Claude hooks (fail-open hardening) ([#1756](https://github.com/bobmatnyc/trusty-tools/pull/1756)) ([`e382abb`](https://github.com/bobmatnyc/trusty-tools/commit/e382abb5c335d1b2429934dc240651cb0d608235))
- idempotent catalog sync — update existing checkout instead of failing on re-clone (closes #1751) ([#1752](https://github.com/bobmatnyc/trusty-tools/pull/1752)) ([`8a70a30`](https://github.com/bobmatnyc/trusty-tools/commit/8a70a3048bd9f699261387a213a10ce67f542a19))
- guided resume restarts a stopped session instead of raw-attaching a dead tmux session (closes #1742) ([#1745](https://github.com/bobmatnyc/trusty-tools/pull/1745)) ([`83f30ba`](https://github.com/bobmatnyc/trusty-tools/commit/83f30ba43e66baefe3715da20281a756997bc7ab))
- hermetic test isolation for managed-session & prune-idle tests (closes #1734) ([#1736](https://github.com/bobmatnyc/trusty-tools/pull/1736)) ([`d0be201`](https://github.com/bobmatnyc/trusty-tools/commit/d0be201928ab9e7c1b7e80c1d23ecb741d38536f))
- include source_id in record_to_json to match record_to_summary (closes #1733) ([#1735](https://github.com/bobmatnyc/trusty-tools/pull/1735)) ([`a901a18`](https://github.com/bobmatnyc/trusty-tools/commit/a901a18a420a31eab0a80f9d6a0c6ccaf5355e0d))
- client source_id field + daemon URL resolution probing for guided tm (closes #1730, closes #1731) ([`2f1eef5`](https://github.com/bobmatnyc/trusty-tools/commit/2f1eef59b04f104bd7444b9fbe1a11837e44cb83))
- redirect guided-default fallback to managed clone, never live checkout ([#1724](https://github.com/bobmatnyc/trusty-tools/pull/1724)) ([#1728](https://github.com/bobmatnyc/trusty-tools/pull/1728)) ([`5a7d9f1`](https://github.com/bobmatnyc/trusty-tools/commit/5a7d9f18844fee4677d0fadfe50fb0946373bd5f))

### Changed

- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))

---

## [0.11.0] — 2026-06-24

### Added

- orphan-GC PID registry + PR A nits ([#1595](https://github.com/bobmatnyc/trusty-tools/pull/1595)) ([#1637](https://github.com/bobmatnyc/trusty-tools/pull/1637)) ([`3886d33`](https://github.com/bobmatnyc/trusty-tools/commit/3886d33e077240bac3e5417427818c41a66e5d8b))
- WI-4 PR A — graceful shutdown hardening (refs #1595) ([#1617](https://github.com/bobmatnyc/trusty-tools/pull/1617)) ([`7f5ed43`](https://github.com/bobmatnyc/trusty-tools/commit/7f5ed43a646fbb1c67f2eb176703a11f696e0712))
- WI-3 SESSCTL Phase 3 — activity observability (closes #1594) ([#1600](https://github.com/bobmatnyc/trusty-tools/pull/1600)) ([`36aebaf`](https://github.com/bobmatnyc/trusty-tools/commit/36aebaf9a43d117dc7441253a52d0b648f00487e))
- WI-2 SESSCTL Phase 2 — sessctl command surface + daemon HTTP endpoints (closes #1593) ([#1599](https://github.com/bobmatnyc/trusty-tools/pull/1599)) ([`3647649`](https://github.com/bobmatnyc/trusty-tools/commit/3647649eb824ff26cdc3524ac89a1004b9e1f9f4))
- WI-1 SESSCTL Phase 1 — backend trait + SessionActor + registry foundation (closes #1592) ([#1598](https://github.com/bobmatnyc/trusty-tools/pull/1598)) ([`68d102a`](https://github.com/bobmatnyc/trusty-tools/commit/68d102a9dc8706a59120923c55bbcbca13e0dae6))
- WI-B group /fleet output by project ([#1588](https://github.com/bobmatnyc/trusty-tools/pull/1588)) ([`9d88c33`](https://github.com/bobmatnyc/trusty-tools/commit/9d88c33880f51d2f857b92f6fad7526bc4ea3c1d))
- WI-A thread repo_url/ref_ through LaunchParams + sessions.launch ([#1587](https://github.com/bobmatnyc/trusty-tools/pull/1587)) ([`64ba815`](https://github.com/bobmatnyc/trusty-tools/commit/64ba815aaf3976e87c686a4e49c8c8ff26833ccb))
- WI-1 isolation regression-guard with version capture (closes #1582, refs #1548) ([#1583](https://github.com/bobmatnyc/trusty-tools/pull/1583)) ([`23d846c`](https://github.com/bobmatnyc/trusty-tools/commit/23d846c35058128d953f71c9aaf4933e1630d67d))
- output-style filesystem deployer for managed config (closes #1553) ([#1580](https://github.com/bobmatnyc/trusty-tools/pull/1580)) ([`46e6f40`](https://github.com/bobmatnyc/trusty-tools/commit/46e6f4092606c98aed2f32459b99cbb28cc5557b))
- tm update + tm rm standalone lifecycle subcommands ([#1578](https://github.com/bobmatnyc/trusty-tools/pull/1578)) ([`b5e4b20`](https://github.com/bobmatnyc/trusty-tools/commit/b5e4b20378d122318bb8bb690911b8c51512d571))
- configurable managed-root via --root / TRUSTY_MPM_ROOT / config.toml ([#1567](https://github.com/bobmatnyc/trusty-tools/pull/1567)) ([`7f781e0`](https://github.com/bobmatnyc/trusty-tools/commit/7f781e0de2e8f9bec2863998f1c0b664c1393c37))
- WI-8 wire trusty-review MCP into tm-global managed config (refs #1548) ([#1563](https://github.com/bobmatnyc/trusty-tools/pull/1563)) ([`a4f1805`](https://github.com/bobmatnyc/trusty-tools/commit/a4f18051a8a64851b37244afda2bbb2fa002c1f0))
- WI-3 managed-session hook-clean + trust-seed + MCP-enable (refs #1548) ([#1555](https://github.com/bobmatnyc/trusty-tools/pull/1555)) ([`66ee38a`](https://github.com/bobmatnyc/trusty-tools/commit/66ee38a58d25e915d7eb98448961b45b42eca390))
- WI-2 deploy bundled agents+skills into managed CLAUDE_CONFIG_DIR (refs #1548) ([#1552](https://github.com/bobmatnyc/trusty-tools/pull/1552)) ([`bce34bd`](https://github.com/bobmatnyc/trusty-tools/commit/bce34bdec7d522d47985104bc0509ced681fbfe1))
- WI-10 managed-session auth — tm login (keychain) + ANTHROPIC_API_KEY/--bare fallback (refs #1548) ([#1551](https://github.com/bobmatnyc/trusty-tools/pull/1551)) ([`539c94a`](https://github.com/bobmatnyc/trusty-tools/commit/539c94ac89a0f38a23b0db9aee9af902d6f89690))
- MVP standalone managed driver — register/load/run with CLAUDE_CONFIG_DIR isolation (refs #1548) ([#1549](https://github.com/bobmatnyc/trusty-tools/pull/1549)) ([`81ca1b0`](https://github.com/bobmatnyc/trusty-tools/commit/81ca1b0dda406113469336f3c492933d07f3bf94))
- NL->repo resolver (WI-5, refs #1517) ([#1535](https://github.com/bobmatnyc/trusty-tools/pull/1535)) ([`222d638`](https://github.com/bobmatnyc/trusty-tools/commit/222d638e6a3f2a11b5d1939299c5a31a3b063904))
- project registry + MCP tools (closes #1519) ([#1520](https://github.com/bobmatnyc/trusty-tools/pull/1520)) ([`53f95c2`](https://github.com/bobmatnyc/trusty-tools/commit/53f95c2d61c1522adfec7e90171187d39523e578))
- wire decommission/inject verbs + graceful no-creds path in action coordinator (closes #1524) ([#1525](https://github.com/bobmatnyc/trusty-tools/pull/1525)) ([`53c49dc`](https://github.com/bobmatnyc/trusty-tools/commit/53c49dc6936c6afa8443e480c1adbc1a399f431b))
- harness-understanding instructions in trusty-agents-common + DOC-21 (closes #1510) ([#1513](https://github.com/bobmatnyc/trusty-tools/pull/1513)) ([`737cddb`](https://github.com/bobmatnyc/trusty-tools/commit/737cddbb6e8908a268604f74a41f361e13f431fc))
- track & tear down ephemeral managed sessions (closes #1508) ([#1509](https://github.com/bobmatnyc/trusty-tools/pull/1509)) ([`3b7d0c9`](https://github.com/bobmatnyc/trusty-tools/commit/3b7d0c9f1225b989687f869b7158bd83b653fc70))
- Slack adapter on the chat-core seam (#1294, epic #1433) ([#1504](https://github.com/bobmatnyc/trusty-tools/pull/1504)) ([`330a2d9`](https://github.com/bobmatnyc/trusty-tools/commit/330a2d918f597b7b91da5aec9d0a9879ec5e5aef))
- web adapter on chat-core seam (refs #1433, #1295, #926) ([#1503](https://github.com/bobmatnyc/trusty-tools/pull/1503)) ([`1c0ccdf`](https://github.com/bobmatnyc/trusty-tools/commit/1c0ccdf93269cd45482a8cb76268c35bfee3247f))
- adopt existing tmux sessions + local-path managed spawn (refs #1433) ([#1502](https://github.com/bobmatnyc/trusty-tools/pull/1502)) ([`be25fff`](https://github.com/bobmatnyc/trusty-tools/commit/be25fff3aa2effdd1f462d2b670ae84e70daf973))
- drive managed fleet from Telegram — free-text→action chat + managed slash commands ([#1501](https://github.com/bobmatnyc/trusty-tools/pull/1501)) ([`5bd5c55`](https://github.com/bobmatnyc/trusty-tools/commit/5bd5c55e985df25049967334b8d3c9cfd0828540))
- add health verb to chat-core catalog + action loop (refs #1433) ([#1498](https://github.com/bobmatnyc/trusty-tools/pull/1498)) ([`5f7b526`](https://github.com/bobmatnyc/trusty-tools/commit/5f7b526982738cc6776792037ac83f9f70be7e72))
- action-capable coordinator chat — self-aware inline verb execution ([#1496](https://github.com/bobmatnyc/trusty-tools/pull/1496)) ([`5c792e7`](https://github.com/bobmatnyc/trusty-tools/commit/5c792e7e8637bd003dbb7bfcc277f263ff4d4008))
- wire STUI slash-dispatch + free-text routing through chat-core (refs #1272, #1276) ([#1494](https://github.com/bobmatnyc/trusty-tools/pull/1494)) ([`62bb3c1`](https://github.com/bobmatnyc/trusty-tools/commit/62bb3c15cf26ee2a6fc3ae2c3d7a0d9df4899273))
- route tm CLI session verbs through chat-core; drop duplicate resolvers (refs #1283) ([#1493](https://github.com/bobmatnyc/trusty-tools/pull/1493)) ([`586f2eb`](https://github.com/bobmatnyc/trusty-tools/commit/586f2eb3032979923869441e1335e0c49a82cdd0))
- chat-core nucleus — shared command layer for session-manager adapters ([#1492](https://github.com/bobmatnyc/trusty-tools/pull/1492)) ([`baaf568`](https://github.com/bobmatnyc/trusty-tools/commit/baaf5689603be72fe7625747c76fddc488d33958))
- typed DaemonClient managed-session methods + refactor tm managed cmds ([#1491](https://github.com/bobmatnyc/trusty-tools/pull/1491)) ([`c5287af`](https://github.com/bobmatnyc/trusty-tools/commit/c5287af26a98b62376914445b1853052f2c0cd6b))
- meta run launches a real Claude Code session + verifies demo artifact (closes #1049, closes #1051) ([#1489](https://github.com/bobmatnyc/trusty-tools/pull/1489)) ([`26fbf15`](https://github.com/bobmatnyc/trusty-tools/commit/26fbf15284136569190796b390597342f9afa717))
- custom-instruction loading for the metaharness (closes #1048) ([#1485](https://github.com/bobmatnyc/trusty-tools/pull/1485)) ([`ee2c498`](https://github.com/bobmatnyc/trusty-tools/commit/ee2c498253f0ec5e9f9d9a25c5f89e9a14d35d9a))
- STUI-1 numbered scrollable session list + keybindings + state preservation (refs #1278) ([#1482](https://github.com/bobmatnyc/trusty-tools/pull/1482)) ([`5bf0009`](https://github.com/bobmatnyc/trusty-tools/commit/5bf00095aef20380f6b32aeb4604ceadbbe41e18))
- standard harness-agnostic inject_text/observe/summarize on SessionControl ([#1461](https://github.com/bobmatnyc/trusty-tools/pull/1461)) ([#1463](https://github.com/bobmatnyc/trusty-tools/pull/1463)) ([`967c892`](https://github.com/bobmatnyc/trusty-tools/commit/967c892236105928ab03b25080cd293392673299))
- periodic + startup orphan-GC reconciling registries vs tmux ls ([#1458](https://github.com/bobmatnyc/trusty-tools/pull/1458)) ([#1462](https://github.com/bobmatnyc/trusty-tools/pull/1462)) ([`aa1c8f8`](https://github.com/bobmatnyc/trusty-tools/commit/aa1c8f8cee680648f8cd5231778f5bbb87a5308d))
- owning tmux Session guard with RAII Drop reaper + test teardown guards (refs #1453, #1459, epic #1452) ([#1460](https://github.com/bobmatnyc/trusty-tools/pull/1460)) ([`ff25808`](https://github.com/bobmatnyc/trusty-tools/commit/ff25808cf8fcb682851d6ba376501cece5674e6f))
- sessions TUI startup banner + service probes (STUI-0) ([#1431](https://github.com/bobmatnyc/trusty-tools/pull/1431)) ([`734b5e5`](https://github.com/bobmatnyc/trusty-tools/commit/734b5e54f2688052cadb2f5e3c26f8ab2d09b139))
- coordinator-context last_summary + summarizing flag (STUI-4) ([#1432](https://github.com/bobmatnyc/trusty-tools/pull/1432)) ([`0fda534`](https://github.com/bobmatnyc/trusty-tools/commit/0fda534e6359d8963ad59044421d4a6a82822afd))
- catalog update-check + rebuild/apply (closes #1408) ([#1429](https://github.com/bobmatnyc/trusty-tools/pull/1429)) ([`41b312a`](https://github.com/bobmatnyc/trusty-tools/commit/41b312a5cb78c469e9c3b0968107c2fd90340203))
- manifest-driven harness provisioning (HR-2, #1407) ([#1427](https://github.com/bobmatnyc/trusty-tools/pull/1427)) ([`f012658`](https://github.com/bobmatnyc/trusty-tools/commit/f012658d26560735b2659a4649cabff41b4ebac8))
- multi-style output + version-fallback injection (HR-4) ([#1412](https://github.com/bobmatnyc/trusty-tools/pull/1412)) ([`77ad339`](https://github.com/bobmatnyc/trusty-tools/commit/77ad33964556158f9816cf9e0fd7de7967ee9114))
- BASE agent content parity + initialPrompt/tier-model injection ([#1411](https://github.com/bobmatnyc/trusty-tools/pull/1411)) ([`0a28b24`](https://github.com/bobmatnyc/trusty-tools/commit/0a28b24550e041139b59c79daead1da3671d1f29))
- wire in-process AgentRunner into meta run orchestrator (closes #1030) ([#1396](https://github.com/bobmatnyc/trusty-tools/pull/1396)) ([`19b972d`](https://github.com/bobmatnyc/trusty-tools/commit/19b972d6e6158ebbc0013a59561c724226d2213a))
- coordinator TUI live session-list polling (Child #2, refs #1274) ([#1386](https://github.com/bobmatnyc/trusty-tools/pull/1386)) ([`44345df`](https://github.com/bobmatnyc/trusty-tools/commit/44345df721f9af648b0b9ac5bdc22d2ee1bcc5dc))
- coordinator TUI skeleton screen + tm coordinator-tui subcommand (Child #1, refs #1272) ([#1383](https://github.com/bobmatnyc/trusty-tools/pull/1383)) ([`2dab8d4`](https://github.com/bobmatnyc/trusty-tools/commit/2dab8d4588a139b7179716f25d8e657548cc5a72))
- wire trusty-code ToolRegistry into tm meta run (WI-2, refs #1045) ([#1384](https://github.com/bobmatnyc/trusty-tools/pull/1384)) ([`0684a7d`](https://github.com/bobmatnyc/trusty-tools/commit/0684a7d2477716904077a7381745f220b5e5c1ed))
- bootstrap tm meta run subcommand (WI-1, refs #1045) ([#1382](https://github.com/bobmatnyc/trusty-tools/pull/1382)) ([`cdaa6c7`](https://github.com/bobmatnyc/trusty-tools/commit/cdaa6c728a7d491984c32fbd27c33e64163b92c9))

### Fixed

- repair daemon::state overseer tests and daemon::api blocking-client panic (closes #1571, closes #1523) ([#1581](https://github.com/bobmatnyc/trusty-tools/pull/1581)) ([`216ee11`](https://github.com/bobmatnyc/trusty-tools/commit/216ee1144ff3173ed5fb59b37383cef10daa30c7))
- managed MVP polish — atomic-save cleanup, credential direction, idempotent .mcp.json ([#1579](https://github.com/bobmatnyc/trusty-tools/pull/1579)) ([`e1721ad`](https://github.com/bobmatnyc/trusty-tools/commit/e1721ad6943f6a32a52a7839aad16c01634cd1de))
- atomic confirm_pair_code with crash-safe claim cleanup (closes #1506) ([#1547](https://github.com/bobmatnyc/trusty-tools/pull/1547)) ([`731d915`](https://github.com/bobmatnyc/trusty-tools/commit/731d91511327124eb795fea5c6ffdfc31db18427))
- stop dropping tokio Runtime in async test context (closes #1521) ([#1522](https://github.com/bobmatnyc/trusty-tools/pull/1522)) ([`ac9365b`](https://github.com/bobmatnyc/trusty-tools/commit/ac9365b3a511ea72fd796ccb6c4e6aafb5dbd25c))
- HTML-escape Telegram command replies (closes #1514) ([#1515](https://github.com/bobmatnyc/trusty-tools/pull/1515)) ([`0e577e3`](https://github.com/bobmatnyc/trusty-tools/commit/0e577e3ab7ff7424e5a5a6242c4094e017170309))
- guard decommission against deleting non-owned workspaces (P0, closes #1511) ([#1512](https://github.com/bobmatnyc/trusty-tools/pull/1512)) ([`435a962`](https://github.com/bobmatnyc/trusty-tools/commit/435a962fc5ae631d5d11afdfc3c771fb9c2d653b))
- supervise Telegram bot + unify pairing-code store (closes #1499, closes #1500) ([#1505](https://github.com/bobmatnyc/trusty-tools/pull/1505)) ([`b5507a3`](https://github.com/bobmatnyc/trusty-tools/commit/b5507a3e59849f56e174af07c415ec448b4a7ee7))
- make telegram bot username configurable via TELEGRAM_BOT_USERNAME (default t_sess_bot) (refs #1433) ([#1497](https://github.com/bobmatnyc/trusty-tools/pull/1497)) ([`897df90`](https://github.com/bobmatnyc/trusty-tools/commit/897df90bdc25156aa8d190e7a4fdb0b5cd1a83c6))
- tmux lifecycle rollbacks — spawn send_line + registry upsert (#1456, #1457) ([#1468](https://github.com/bobmatnyc/trusty-tools/pull/1468)) ([`e471ee2`](https://github.com/bobmatnyc/trusty-tools/commit/e471ee2ead502b013f41ea59e40e75c13475ba45))
- tmux lifecycle — DELETE kills session + graceful-shutdown reaper (#1454, #1455) ([#1466](https://github.com/bobmatnyc/trusty-tools/pull/1466)) ([`4c53699`](https://github.com/bobmatnyc/trusty-tools/commit/4c5369923e592288e9b0e5d41dec028ec345078b))

### Changed

- WI-2 review nits — single missing-source hint + document deploy layout (refs #1548) ([#1556](https://github.com/bobmatnyc/trusty-tools/pull/1556)) ([`404b874`](https://github.com/bobmatnyc/trusty-tools/commit/404b8744e72ffeec07a2f1e12cb3eeeb217b38b1))
- rename CLI 'session' command group to 'sessions' (closes #1394) ([#1395](https://github.com/bobmatnyc/trusty-tools/pull/1395)) ([`864b006`](https://github.com/bobmatnyc/trusty-tools/commit/864b0062bfd0a3d913855c45fa9d246bca13634f))
- rename `tm coordinator-tui` → `tm session tui` + move coordinator API under /api/v1/sessions ([#1393](https://github.com/bobmatnyc/trusty-tools/pull/1393)) ([`749e7dd`](https://github.com/bobmatnyc/trusty-tools/commit/749e7dd4bad08ff8f93c04bb7c3d36991221f79b))

---

## [0.10.0] — 2026-06-17

### Fixed (closes #1373)

- **Sessions now register + pin their own project's trusty-search index.** At
  session launch `prepare_session` derives the project's canonical index id
  (git-root basename, via the shared `trusty_common::derive_index_id`),
  best-effort find-or-creates it in the running trusty-search daemon
  (`POST /indexes`), and injects the `trusty-search` MCP stub **pinned** to that
  id (`serve --index <id>`). A bare `search`/`grep` therefore resolves to the
  session's own project index instead of letting the LLM guess — which
  routinely picked the wrong (usually persistent `claude-mpm`) index. The
  daemon-unreachable case is graceful: it logs a warning and still pins the
  stub (the index is created on first reindex); an empty derived id falls back
  to the unpinned `serve` stub. Either way the session always launches.

## [0.9.0] — 2026-06-16

### Release

- **First monorepo publish.** This is the first `trusty-mpm` release published
  from the unified `trusty-tools` workspace. It supersedes the stale `0.8.1`
  on crates.io, which was published from the now-archived standalone repo.

### Fixed

- **Standalone build break:** `daemon/mcp_console.rs` imports
  `trusty_common::console_metrics` unconditionally, but that module is gated
  behind trusty-common's `console-metrics` feature. trusty-mpm's main
  `trusty-common` dependency now enables `console-metrics`, so
  `cargo check -p trusty-mpm` and `cargo publish` no longer fail to resolve
  the module. (Workspace feature-unification previously masked this under
  `cargo test`.)

## [0.8.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-mpm` now produces
  `tm` and `trusty-mpm` only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [0.5.0] — 2026-05-28

### Added: `tm services` — canonical service-discovery CLI (issue #339)

**New subcommand**: `tm services <action>` — replaces ad-hoc `lsof`/`curl`/`ps`
patterns for discovering the port, health, and status of every trusty-* daemon.

#### Subcommands

| Command | Description |
|---------|-------------|
| `tm services list [--json]` | Table of all declared services with running/down status, port, version, and health |
| `tm services status <name> [--json]` | Detailed block for one service |
| `tm services port <name>` | Print just the port number (scriptable) |
| `tm services url <name>` | Print the full base URL |
| `tm services health <name>` | Probe the `/health` endpoint; exit 0 if healthy |
| `tm services log <name>` | Print the log file path if it exists |
| `tm services init [--force]` | Write the default manifest to `~/.claude-mpm/services.yaml` |
| `tm services restart <name>` | Execute the manifest `restart_cmd` |

#### Manifest

Default manifest embedded in the binary covers 6 services:

- `trusty-search` — port 7878, `/health` confirmed
- `trusty-analyze` — port 7879, `/health` confirmed
- `trusty-mpm-daemon` — port 7880, `/health` confirmed at `daemon/api.rs:74`
- `trusty-memory` — dynamic port (7070-7079) via `~/.trusty-memory/http_addr`
- `trusty-embedderd` — UDS sidecar, pgrep-only (no HTTP surface)
- `trusty-bm25-daemon` — UDS sidecar, pgrep-only (no HTTP surface)

Custom manifests can be placed at `~/.claude-mpm/services.yaml` (use `tm services init`).

#### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Running/healthy (list always exits 0) |
| 1 | Service declared but down, or health probe failed |
| 2 | Service name not in manifest |

#### Scriptable usage

```bash
PORT=$(tm services port trusty-search)
URL=$(tm services url trusty-search)
tail -f $(tm services log trusty-search)
```

#### Architecture

- `crates/trusty-mpm/src/services/manifest.rs` — `ServicesManifest`, `ServiceDecl`,
  `PortDiscovery` enum, `ManifestValidationError` (thiserror)
- `crates/trusty-mpm/src/services/discoverer.rs` — `Discoverer` with 5-second
  TTL cache; `ProcessProber`/`PortProber`/`HttpProber`/`VersionRunner` trait
  seams for unit testing
- `crates/trusty-mpm/assets/default-services.yaml` — embedded default manifest

**Tests**: 21 new unit tests (8 manifest + 13 discoverer, all mocked) + 11 CLI
parse tests + 2 ignore-gated integration smoke tests.

---

## [consolidation] — 2026-05-26

**Combined 7 trusty-mpm-\* sub-crates into one crate with feature-gated `[[bin]]` targets.**

### Summary

The following sub-crates have been merged into this unified `trusty-mpm` crate:

| Former crate | Now lives in |
|---|---|
| `trusty-mpm-core` | `crates/trusty-mpm/src/core/` |
| `trusty-mpm-client` | `crates/trusty-mpm/src/client/` |
| `trusty-mpm-mcp` | `crates/trusty-mpm/src/mcp/` (feature: `mcp`) |
| `trusty-mpm-daemon` | `crates/trusty-mpm/src/daemon/` (feature: `daemon`) |
| `trusty-mpm-cli` | `crates/trusty-mpm/src/bin/tm.rs` (feature: `cli`) |
| `trusty-mpm-tui` | `crates/trusty-mpm/src/tui/` (feature: `tui`) |
| `trusty-mpm-telegram` | `crates/trusty-mpm/src/telegram/` (feature: `telegram`) |

The Tauri desktop GUI (`trusty-mpm-gui`) remains as a separate crate because
it owns `build.rs` (invoking `tauri_build::build()`) and `tauri.conf.json` — files
that cannot co-exist with a generic Cargo crate build system. The `gui` feature of
this crate wraps it as an optional path dependency.

### Workspace crate count
- Removed: 7 crates (`trusty-mpm-core`, `trusty-mpm-mcp`, `trusty-mpm-daemon`,
  `trusty-mpm-client`, `trusty-mpm-cli`, `trusty-mpm-tui`, `trusty-mpm-telegram`)
- Added: 1 crate (`trusty-mpm`)
- Net change: 28 → 22 workspace members

### Feature flags

| Feature | What it enables |
|---|---|
| `default` | `cli` + `daemon` (the common install path) |
| `cli` | `tm` / `trusty-mpm` CLI binary (implies `daemon`, `tui`, `telegram`) |
| `daemon` | `trusty-mpmd` daemon binary + daemon library module (implies `mcp`) |
| `mcp` | MCP server library module |
| `tui` | `trusty-mpm-tui` shim binary + TUI library module |
| `telegram` | `trusty-mpm-telegram` shim binary + Telegram library module |
| `gui` | `trusty-mpm-gui` shim binary (wraps the separate `trusty-mpm-gui` crate) |

### Public API surface

All public types, traits, and functions are preserved. The only change is the
import path: code that previously imported from `trusty_mpm_core`, `trusty_mpm_client`,
etc. should now import from the corresponding submodule of `trusty_mpm`:

```rust
// Before
use trusty_mpm_core::session::{Session, SessionId};
use trusty_mpm_client::DaemonClient;

// After
use trusty_mpm::core::session::{Session, SessionId};
use trusty_mpm::client::DaemonClient;
```

### Deprecation notes

The following crate names are no longer published:
- `trusty-mpm-core`
- `trusty-mpm-mcp`
- `trusty-mpm-daemon`
- `trusty-mpm-client`
- `trusty-mpm-cli`
- `trusty-mpm-tui`
- `trusty-mpm-telegram`

All functionality is available under `trusty-mpm` with the appropriate feature flags.

## [0.4.0] and prior

See the individual crate changelogs in the former sub-crate directories (available
in git history as `crates/trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}/`).
