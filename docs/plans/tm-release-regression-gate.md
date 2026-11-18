# tm release regression gate: read-only command inventory (#11470)

Start 2026-11-18T16:11:17Z. Inventory only; nothing is wired in.
Binary under test: `$HOME/.cargo/bin/tm`, `tm --version` = `trusty-mpm 1.14.8`.
(`tm` on `PATH` is a mise shim that errors "not a valid shim", so the cargo-installed binary was used.)

Method and limits. Every `--help` ran under `env -i HOME=<fresh mktemp> PATH=/usr/bin:/bin:$HOME/.cargo/bin TERM=dumb`.
No `tm` subcommand was executed. Classification and expected exit codes come from `--help` text and source reading, not from runs.
A trailing `?` on an exit code means the code is not backed by source or help text. Those need a calibration run (open question 1).
Seconds are estimates, not measurements.

## Command count

| Measure | Count |
|---|---|
| Top-level commands in `--help` (excluding auto `help`) | 69 |
| All command paths in `--help` (recursive) | 270 |
| Group nodes (have subcommands) | 41 |
| Leaf commands in `--help` | 229 |
| Hidden or non-`--help` leaves added from source | 7 (`session <verb>` alias tree counted once, `internal-spawn-disclaimed`, `sessions managed-stop`, `sessions runtime-stop`, `sessions managed-resume`, `secrets __exec-stub`, external subcommand `tm <url>`) |
| Extra variant row | 1 (`wait --for check`, the NETWORK mode of the `wait` leaf) |
| Rows in the table | 237 |

Per class (237 rows): SAFE 76, SANDBOX 71, NETWORK 12, SKIP 78.
SANDBOX split by needed sandbox: daemon only 54, daemon + private tmux 9, private tmux only 2, other 6.
26 of the 71 SANDBOX rows are negative cases (unknown session id, expected non-zero), because a positive case would need a live `claude` session.

## Source versus `--help` mismatches

1. Hidden, not in `--help`: top-level `session` (`cli/mod.rs:269`, deprecated alias of `sessions`, identical dispatch, notice on stderr); `internal-spawn-disclaimed` (`cli/mod.rs:303`); `sessions managed-stop`, `runtime-stop`, `managed-resume` (`cli/actions/session.rs:310-332`); `secrets __exec-stub` (`cli/actions/secrets.rs:205`).
2. `Command::External(Vec<String>)` (`cli/mod.rs:1718`) accepts any unknown first token. `tm <url>` and `tm <owner/repo>` are real invocations that no `--help` walk shows. Rejection of retired names happens after parse (`tests.rs:1577`).
3. `infer_subcommands` is on and walks hidden commands too. `commands/spawn_disclaimed.rs:36` guards `tm int <program>` with `invoked_literally`. A gate that tests abbreviations must expect that refusal.
4. Aliases: `coordinator` = `sm`, `session-manager`, `coord` (`cli/mod.rs:997`); `--account` = `--user`, `--u`; `deliverables add` and `milestones add` accept `create`; `sessions stop` carries `kill` (`cli/actions/session.rs:41`).
5. Path shape: `projects config <NAME> set|unset|tags` takes `NAME` before the verb. A path walker yields `projects config set`, which is not a runnable invocation.
6. The source `Command` enum has 72 variants: 69 visible, `Session`, `InternalSpawnDisclaimed`, `External`. No visible variant is missing from `--help`.
7. `help` is clap's auto subcommand and was excluded from the walk.

All paths are relative to `crates/trusty-mpm/src/bin/tm/` unless they start with `core/` or `content/`.

## Table

Conventions. `$H` is the temp HOME. D = a daemon from `scripts/sandbox_daemon.sh`; DT = that daemon plus its private tmux (`TMUX_TMPDIR=$D/data/tmux`); T = a private tmux server only (`TMUX_SOCKET=<name>` with `tmux -L <name>` and a private `TMUX_TMPDIR`). Clients under D need `TRUSTY_MPM_SOCKET=<printed socket>`.
`-` in the smoke column means the command is not run. "after X" means X must have run in the same `$H`.

| command path | class | smoke invocation | expected exit | sandbox / skip reason | est. s |
|---|---|---|---|---|---|
| `status` | SANDBOX | `status` | 0 | D: probes the daemon socket | 1 |
| `start` | SKIP | - | - | spawns a daemon; under TRUSTY_SANDBOX it refuses (#9556); sandbox_daemon.sh is the only sanctioned launcher | 0 |
| `serve` | SKIP | - | - | alias of start; `serve --stdio` MCP bridge also autostarts a daemon | 0 |
| `stop` | SKIP | - | - | sysinfo walk of the HOST process table, SIGTERM/SIGKILL on every `tm daemon` (commands/daemon.rs:377), not scoped by HOME | 0 |
| `restart` | SKIP | - | - | stop (host-wide kill) then start | 0 |
| `project init` | SANDBOX | `project init (cwd = $H/proj, git repo)` | 0 | D: registers with daemon; deprecated alias of projects register | 1 |
| `project list` | SANDBOX | `project list` | 0 | D: deprecated alias of projects list | 1 |
| `project info` | SANDBOX | `project info (cwd = $H/proj)` | 0 | D: after project init | 1 |
| `project trust` | SAFE | `project trust --dir $H/proj` | 0 | writes consent file under temp HOME; then --revoke | 1 |
| `sessions start` | SKIP | - | - | launches a Claude session (claude binary, credentials) | 0 |
| `sessions stop` | SANDBOX | `sessions stop nosuch` | 1? | DT: negative case, unknown id | 1 |
| `sessions list` | SANDBOX | `sessions list` | 0 | D | 1 |
| `sessions tui` | SKIP | - | - | interactive ratatui, needs a TTY | 0 |
| `sessions clean` | SANDBOX | `sessions clean` | 0 | D: reaps dead sessions of the sandbox registry | 1 |
| `sessions info` | SANDBOX | `sessions info nosuch` | 1? | D: negative case | 1 |
| `sessions instructions` | SAFE | `sessions instructions --dir $H/proj` | 0? | composes launch text locally (daemon use unverified) | 1 |
| `sessions events` | SANDBOX | `sessions events nosuch` | 1? | D: negative case | 1 |
| `sessions breakers` | SANDBOX | `sessions breakers` | 0 | D | 1 |
| `sessions pause` | SANDBOX | `sessions pause nosuch` | 1? | DT: negative case | 1 |
| `sessions resume` | SANDBOX | `sessions resume nosuch` | 1? | DT: negative case | 1 |
| `sessions run` | SANDBOX | `sessions run nosuch "true"` | 1? | DT: negative case; a positive case would type into a pane | 1 |
| `sessions output` | SANDBOX | `sessions output nosuch` | 1? | DT: negative case | 1 |
| `sessions new` | SKIP | - | - | spawns a managed session: tmux + claude + credentials | 0 |
| `sessions ls` | SANDBOX | `sessions ls --no-prune` | 0 | D: --no-prune is the read that changes nothing | 1 |
| `sessions activity` | SANDBOX | `sessions activity nosuch` | 1? | D: negative case | 1 |
| `sessions send` | SANDBOX | `sessions send nosuch hi` | 1? | DT: negative case (injects text into a pane) | 1 |
| `sessions answer` | SANDBOX | `sessions answer nosuch yes` | 1? | DT: negative case | 1 |
| `sessions attach` | SANDBOX | `sessions attach nosuch` | 1? | DT: only prints the attach command; negative case | 1 |
| `sessions decommission` | SANDBOX | `sessions decommission nosuch` | 1? | D: removes a workspace dir; negative case only | 1 |
| `sessions delete` | SANDBOX | `sessions delete nosuch` | 1? | D: negative case | 1 |
| `sessions archive` | SANDBOX | `sessions archive nosuch` | 1? | D: moves a project dir to $H/.trusty-tools/archive; negative case | 1 |
| `sessions rename` | SANDBOX | `sessions rename nosuch new` | 1? | D: negative case | 1 |
| `sessions rebind` | SANDBOX | `sessions rebind nosuch` | 1? | DT: negative case | 1 |
| `sessions prune-idle` | SANDBOX | `sessions prune-idle` | 0 | D: empty registry, no-op | 1 |
| `sessions decommission-ephemeral` | SANDBOX | `sessions decommission-ephemeral` | 0 | D: tears down EVERY ephemeral session; safe only on the pinned sandbox socket | 1 |
| `sessions catchup` | SAFE | `sessions catchup --json` | 0? | reads git status + fail-open trusty-memory socket (absent in temp HOME) | 1 |
| `sessions prune` | SANDBOX | `sessions prune --state dead` | 0 | D: state values per --help; empty registry | 1 |
| `sessions sync-assets` | SANDBOX | `sessions sync-assets nosuch` | 1? | D: negative case | 1 |
| `sessions prune-worktrees` | SANDBOX | `sessions prune-worktrees` | 0? | D: deletes worktree dirs; empty registry; check for a dry-run flag | 1 |
| `sessions reconcile-worktrees` | SANDBOX | `sessions reconcile-worktrees --json` | 0 | D: read-only report | 1 |
| `sessions adopt-worktree` | SANDBOX | `sessions adopt-worktree --as nosuch $H/proj` | 1? | D: negative case | 1 |
| `sessions disk` | SANDBOX | `sessions disk --json` | 0 | D | 2 |
| `projects list` | SANDBOX | `projects list` | 0 | D | 1 |
| `projects register` | SANDBOX | `projects register --repo-url https://example.invalid/o/r.git gate-p` | 0 | D: registry write in the sandbox daemon; a clone is NOT triggered by register (unverified) | 1 |
| `projects show` | SANDBOX | `projects show gate-p` | 0 | D: after register | 1 |
| `projects status` | SANDBOX | `projects status gate-p` | 0 | D | 1 |
| `projects decommission` | SANDBOX | `projects decommission gate-p` | 0 | D: last in the chain | 1 |
| `projects config set` | SANDBOX | `projects config gate-p set FIELD VALUE` | 0? | D: FIELD from --help; NAME precedes the verb | 1 |
| `projects config unset` | SANDBOX | `projects config gate-p unset FIELD` | 0? | D | 1 |
| `projects config tags` | SANDBOX | `projects config gate-p tags --add t` | 0? | D | 1 |
| `projects deliverables list` | SANDBOX | `projects deliverables list gate-p` | 0 | D | 1 |
| `projects deliverables add` | SANDBOX | `projects deliverables add gate-p --name d --kind K --estimate E` | 0? | D: KIND/ESTIMATE per --help; alias `create` | 1 |
| `projects deliverables show` | SANDBOX | `projects deliverables show gate-p ID` | 0 | D: ID from add output | 1 |
| `projects deliverables set-status` | SANDBOX | `projects deliverables set-status gate-p ID STATUS` | 0? | D: state machine edge from proposed | 1 |
| `projects milestones list` | SANDBOX | `projects milestones list gate-p` | 0 | D | 1 |
| `projects milestones add` | SANDBOX | `projects milestones add gate-p --name m --target-date 2030-01-01` | 0 | D: alias `create` | 1 |
| `projects milestones show` | SANDBOX | `projects milestones show gate-p ID` | 0 | D | 1 |
| `manager status` | SANDBOX | `manager status` | 0 | D: deterministic, no LLM | 1 |
| `manager digest` | SKIP | - | - | calls an inference provider (needs key, outward); deterministic fallback exists but then the smoke tests the fallback only | 0 |
| `manager chat` | SKIP | - | - | LLM turn, needs a provider key | 0 |
| `manager route` | SKIP | - | - | advisory route via LLM (needs a provider key); not verified to have a no-LLM path | 0 |
| `events` | SANDBOX | `events` | 0 | D | 1 |
| `doctor` | SANDBOX | `doctor` | 0 | D: any FAIL row exits non-zero, so a clean HOME with no installed content fails; run after install; runs read-only `launchctl list`; `--network` row spawns gcloud, never use | 8 |
| `validate` | SAFE | `validate --path $H/ws` | 0? | local diff engine; an empty workspace likely reports drift (pin on calibration run) | 1 |
| `hooks clean` | SAFE | `hooks clean (cwd = $H/proj)` | 0 | edits only project .claude/settings*.json in cwd | 1 |
| `agent list` | SAFE | `agent list --json` | 0 | after install | 1 |
| `agent show` | SAFE | `agent show engineer --json` | 0 | after install; name from agent list | 1 |
| `generate capabilities` | SKIP | - | - | writes content/skills/tm-capabilities* in a source checkout; `--check` needs the source tree; CI already runs scripts/check_capabilities.sh | 0 |
| `health` | SANDBOX | `health` | 0 | D | 1 |
| `tui` | SKIP | - | - | interactive ratatui, needs a TTY | 0 |
| `telegram pair` | SANDBOX | `telegram pair` | 0? | D: POST /pair/request on the sandbox daemon; makes no Telegram call | 1 |
| `telegram status` | SANDBOX | `telegram status` | 0 | D | 1 |
| `telegram start` | SKIP | - | - | starts the bot: bot token, outward polling | 0 |
| `telegram stop` | SKIP | - | - | `pkill -f "telegram start"` kills host processes, not scoped by HOME (telegram.rs:106) | 0 |
| `slack start` | SKIP | - | - | starts the Socket Mode bot: Slack tokens, outward | 0 |
| `slack stop` | SAFE | `slack stop` | 0? | PID-file under $HOME/.trusty-mpm/slack.pid; none exists in temp HOME | 1 |
| `install` | SAFE | `install` | 0 | writes $H/.trusty-mpm/framework and $H/.claude; the fixture for later steps | 4 |
| `reinstall` | SAFE | `reinstall` | 0 | without --binary; `--binary` runs cargo install (reinstall.rs:280) and is never smoked | 4 |
| `hook` | SAFE | `hook --pm-guard < fixture-pretooluse.json` | 0? | local classify, no daemon; fixture on stdin; deny verdicts may use another exit | 1 |
| `prompt-feedback` | SAFE | `prompt-feedback --summary` | 0 | reads empty ledger under temp HOME | 1 |
| `divert bulk-read` | SKIP | - | - | calls a cheap worker model (credential, outward) | 0 |
| `memory recall` | SKIP | - | - | needs a trusty-memory daemon; no sanctioned memory sandbox script exists | 0 |
| `memory remember` | SKIP | - | - | writes a palace; same | 0 |
| `memory note` | SKIP | - | - | writes a palace; same | 0 |
| `memory forget` | SKIP | - | - | deletes a drawer; same | 0 |
| `memory rename` | SKIP | - | - | moves a palace; same | 0 |
| `memory import` | SKIP | - | - | bulk write into a palace; same | 0 |
| `memory import-auto-memory` | SKIP | - | - | reads Claude's auto-memory store, writes a palace | 0 |
| `compress` | SAFE | `printf 'a\n' piped to: compress --tool "cargo test"` | 0 | stdin filter | 1 |
| `wait` | SAFE | `wait --for file --path $H/f --timeout 5 (file pre-created)` | 0 | `--for check` is NETWORK, see wait --for check | 1 |
| `build-lease` | SAFE | `build-lease status` | 0 | flock slots under $H/.trusty-mpm/build-slots; a build after `--` is never smoked | 1 |
| `worktrees` | SAFE | `worktrees --json --no-size --no-fetch` | 0 | ledger under temp HOME; --no-fetch avoids git fetch | 1 |
| `daemon` | SANDBOX | `scripts/sandbox_daemon.sh --dry-run` | 0 | this IS the fixture; `tm daemon` itself runs only through the script | 1 |
| `supervisor` | SKIP | - | - | 24/7 fleet observer over tmux panes | 0 |
| `launch` | SKIP | - | - | deploys and starts claude in tmux; credentials, TTY | 0 |
| `connect` | SKIP | - | - | starts or attaches a claude session | 0 |
| `attach` | SKIP | - | - | opens the multipane TUI; needs a TTY | 0 |
| `optimizer status` | SAFE | `optimizer status` | 0 | after install | 1 |
| `optimizer set` | SAFE | `optimizer set LEVEL` | 0 | LEVEL from --help; rewrites the policy file in temp HOME | 1 |
| `fleet init` | SKIP | - | - | creates the Architect project and starts its session | 0 |
| `fleet status` | SAFE | `fleet status --json` | 0? | read-only; exit when not set up unverified | 1 |
| `fleet relaunch` | SKIP | - | - | restarts the Architect claude in tmux | 0 |
| `fleet bind` | SKIP | - | - | binds the running Architect claude; needs that session | 0 |
| `fleet project add` | SANDBOX | `fleet project add gate-p` | 0? | D: needs registered project | 1 |
| `fleet project remove` | SANDBOX | `fleet project remove KEY` | 0? | D | 1 |
| `fleet project list` | SANDBOX | `fleet project list` | 0 | D: lists live sessions | 1 |
| `architect poll` | SANDBOX | `architect poll (TMUX_SOCKET=gate, TMUX_TMPDIR set)` | 0? | T: reads tmux via TMUX_SOCKET; unset means the default (live) server | 2 |
| `architect classify` | SAFE | `architect classify fixture-pane.txt` | 0 | pure parse | 1 |
| `architect input-state` | SANDBOX | `architect input-state gate:0.0 (TMUX_SOCKET=gate)` | 1? | T: needs a tmux server; negative target | 2 |
| `architect quiet` | SAFE | `architect quiet --file fixture-state.json` | 0 | reads a state file | 1 |
| `architect self-ctx` | SAFE | `architect self-ctx $H/empty` | 0? | reads transcripts under a dir | 1 |
| `architect dashboard get` | SAFE | `architect dashboard get --root $H/dash C ID` | 0 | in-process store; exit 1/2/3 per #10063 | 1 |
| `architect dashboard list` | SAFE | `architect dashboard list --root $H/dash C` | 0 | ;; 1 |  |
| `architect dashboard set` | SAFE | `architect dashboard set --root $H/dash --json '{}' C ID` | 0 | writes only $H/dash | 1 |
| `architect dashboard update` | SAFE | `architect dashboard update --root $H/dash --json '{}' C ID` | 0 | after set | 1 |
| `architect dashboard delete` | SAFE | `architect dashboard delete --root $H/dash C ID` | 0 | ;; 1 |  |
| `architect dashboard batch` | SAFE | `architect dashboard batch --root $H/dash ops.json` | 0 | ;; 1 |  |
| `architect dashboard pruned` | SAFE | `architect dashboard pruned --root $H/dash` | 0 | ;; 1 |  |
| `architect dashboard validate` | SAFE | `architect dashboard validate --root $H/dash` | 0 | exit 1 on a problem | 1 |
| `architect dashboard fsck` | SAFE | `architect dashboard fsck --root $H/dash` | 0 | exit 1 on a problem | 1 |
| `architect dashboard tally` | SAFE | `architect dashboard tally --root $H/dash` | 0 | ;; 1 |  |
| `architect dashboard footer` | SAFE | `architect dashboard footer --root $H/dash` | 0 | ;; 1 |  |
| `architect dashboard ids` | SAFE | `architect dashboard ids --root $H/dash P` | 0 | ;; 1 |  |
| `architect up` | SKIP | - | - | brings up the Architect session: tmux + claude, then attaches | 0 |
| `architect attach` | SKIP | - | - | attaches to the Architect tmux session; TTY | 0 |
| `architect restart` | SKIP | - | - | restarts the Architect behind a checkpoint gate | 0 |
| `presence local` | SKIP | - | - | Touch ID / login password prompt, owner present | 0 |
| `presence request` | SKIP | - | - | asks an enrolled device to approve; outward, owner present | 0 |
| `presence wait` | SKIP | - | - | waits for a human grant | 0 |
| `presence devices add` | SKIP | - | - | passkey enrolment, incomplete (#8878), owner present | 0 |
| `presence devices list` | SAFE | `presence devices list` | 0? | reads device records under temp HOME; Keychain use unverified | 1 |
| `presence devices revoke` | SKIP | - | - | needs Touch ID or login password | 0 |
| `dbatg init` | SAFE | `dbatg init --code GATE --project $H/proj --session s1` | 0 | store file under the project | 1 |
| `dbatg add` | SAFE | `dbatg add --title t --project $H/proj --session s1 T` | 0 | ;; 1 |  |
| `dbatg update` | SAFE | `dbatg update --project $H/proj --session s1 GATE-T1` | 0? | flags per --help | 1 |
| `dbatg close` | SAFE | `dbatg close --outcome ok --project $H/proj --session s1 GATE-T1` | 0 | ;; 1 |  |
| `dbatg batch` | SAFE | `dbatg batch --project $H/proj --session s1 < ops.json` | 0 | ;; 1 |  |
| `dbatg prune` | SAFE | `dbatg prune --project $H/proj --session s1` | 0 | ;; 1 |  |
| `dbatg show` | SAFE | `dbatg show --brief --project $H/proj --session s1` | 0 | ;; 1 |  |
| `env set` | SKIP | - | - | value comes from the login Keychain; needs the Architect binding and a pm-guard grant | 0 |
| `env keys` | SKIP | - | - | refused without the Architect binding and a pm-guard grant | 0 |
| `secrets set` | SKIP | - | - | value from the clipboard; trusty-secrets backend / Keychain | 0 |
| `secrets list` | SKIP | - | - | needs the trusty-secrets socket; sandbox_secrets_console.sh is a different harness (#9793) | 0 |
| `secrets remove` | SKIP | - | - | deletes a vault key | 0 |
| `secrets import` | SKIP | - | - | loads a dotenv into the vault | 0 |
| `secrets copy` | SKIP | - | - | vault to vault copy; Keychain | 0 |
| `secrets assign` | SKIP | - | - | vault to vault copy; Keychain | 0 |
| `secrets doctor` | SKIP | - | - | probes the secrets socket and Keychain backend | 0 |
| `secrets resolve` | SKIP | - | - | prints a value, only inside a `secrets exec` grant | 0 |
| `secrets exec` | SKIP | - | - | resolves vault keys into a child env | 0 |
| `content install` | SAFE | `content install --from $PUBLISHED_BUNDLE` | 0 | no network; the "published content" fixture of the gate | 2 |
| `content update` | NETWORK | `content update` | 0 | 2 REST calls (git/matching-refs listing + releases/tags/{tag}) plus 3 asset downloads; unauthenticated = 60/h/IP | 4 |
| `content status` | SAFE | `content status` | 0 | after content install | 1 |
| `overseer status` | SAFE | `overseer status` | 0 | reads config | 1 |
| `coordinator serve` | SANDBOX | `coordinator serve < initialize.json` | 0? | D: JSON-RPC over stdio to the daemon; aliases sm, session-manager, coord | 2 |
| `services list` | SAFE | `services list --json` | 0 | after services init | 1 |
| `services status` | SANDBOX | `services status NAME` | 0? | probes the service port on localhost (host port, not HOME-scoped) | 1 |
| `services port` | SAFE | `services port NAME` | 0 | manifest read | 1 |
| `services url` | SAFE | `services url NAME` | 0 | manifest read | 1 |
| `services health` | SKIP | - | - | HTTP probe of a manifest port on the host; a live service of the owner could answer | 0 |
| `services log` | SAFE | `services log NAME` | 0? | prints a path | 1 |
| `services init` | SAFE | `services init` | 0 | writes $H/.claude-mpm/services.yaml | 1 |
| `services restart` | SKIP | - | - | `sh -c <restart_cmd>` from the manifest (services.rs:153) | 0 |
| `repair deploy` | SAFE | `repair deploy` | 0 | ~/.claude of temp HOME | 2 |
| `repair push-guard` | SAFE | `repair push-guard --path $H/proj --dry-run` | 0 | ;; 1 |  |
| `repair session-store` | SAFE | `repair session-store --path $H/s.json --dry-run` | 0? | ;; 1 |  |
| `repair savings-ledger` | SAFE | `repair savings-ledger --root $H --dry-run` | 0 | ;; 1 |  |
| `repair delegation` | SAFE | `repair delegation --list $H` | 0 | ;; 1 |  |
| `auth set-token` | SKIP | - | - | stores a token; macOS Keychain, not HOME-scoped | 0 |
| `auth clear-token` | SKIP | - | - | Keychain delete | 0 |
| `auth status` | SKIP | - | - | reads the Keychain entry | 0 |
| `catalog sync` | NETWORK | `catalog sync --force` | 0 | 0 REST calls, 1 git clone over HTTPS of the claude-mpm repo | 8 |
| `catalog ls` | SAFE | `catalog ls --json` | 0 | after catalog sync | 1 |
| `catalog status` | SAFE | `catalog status --json` | 0 | ;; 1 |  |
| `catalog apply` | NETWORK | `catalog apply` | 0 | sync (cached TTL after catalog sync: 0 calls) then local redeploy | 3 |
| `ticket` | SKIP | - | - | issue to branch to PR to close: writes to GitHub | 0 |
| `issue seed-labels` | SKIP | - | - | creates labels in a real repo | 0 |
| `issue transition` | SKIP | - | - | edits issue labels | 0 |
| `issue current` | NETWORK | `issue current N --repo bobmatnyc/trusty-tools` | 0 | 1 gh call (issue view) | 2 |
| `issue states` | SAFE | `issue states` | 0 | YAML only | 1 |
| `issue standard` | SAFE | `issue standard` | 0 | config only, no gh | 1 |
| `issue seed-config` | SAFE | `issue seed-config` | 0 | writes the user config in temp HOME | 1 |
| `issue audit` | NETWORK | `issue audit N` | 0 | 2 to 4 gh calls (issue view x2, plus board GraphQL if configured) | 4 |
| `issue epic create` | SKIP | - | - | files issues | 0 |
| `issue epic sync` | SKIP | - | - | edits a tracker issue body | 0 |
| `issue epic defer` | SKIP | - | - | edits a tracker issue body | 0 |
| `issue epic close` | SKIP | - | - | closes an issue | 0 |
| `issue board init` | NETWORK | `issue board init --owner O --repo O/R (dry run)` | 0 | dry run unless --apply; 1 to 3 GraphQL calls | 4 |
| `issue board apply` | NETWORK | `issue board apply (dry run)` | 0 | dry run unless --apply; 2 to 4 calls | 4 |
| `issue board check` | NETWORK | `issue board check` | 0 | 2 to 4 calls (GraphQL + `api --include`) | 4 |
| `issue board show` | SAFE | `issue board show` | 0 | embedded spec | 1 |
| `issue board add` | SKIP | - | - | adds an issue to a real board | 0 |
| `issue repair` | SKIP | - | - | edits issue labels | 0 |
| `pr open` | SKIP | - | - | opens a pull request | 0 |
| `pr merge` | SKIP | - | - | squash-merges a PR | 0 |
| `pr queue-check` | NETWORK | `pr queue-check N --repo O/R` | 0 | 3 calls with an explicit PR (pr view, branch, required_status_checks); without PR: 1 list + 3 per open PR | 4 |
| `pr cleanup` | SKIP | - | - | deletes branches and worktrees | 0 |
| `watch poll` | SKIP | - | - | dispatches agents for matched issues | 0 |
| `watch listen` | SKIP | - | - | long-running dispatcher | 0 |
| `register` | SAFE | `register https://github.com/o/r gate-a` | 0 | persists alias to URL, no clone (standalone.rs:37) | 1 |
| `ls` | SANDBOX | `ls --plain --no-prune` | 0 | D: non-interactive flags only; bare `ls` is a picker | 1 |
| `f` | SKIP | - | - | interactive filter picker, needs a TTY | 0 |
| `load` | NETWORK | `load gate-a` | 0 | git clone, 0 REST calls; needs a clone-able alias URL | 6 |
| `run` | SKIP | - | - | starts a claude session for a repo | 0 |
| `path` | SAFE | `path gate-a` | 1? | negative until load; prints path after | 1 |
| `shell-init` | SAFE | `shell-init zsh` | 0 | prints text | 1 |
| `login` | SKIP | - | - | Keychain login prompt | 0 |
| `rm` | SAFE | `rm gate-a` | 0 | deletes only the alias project dir under temp HOME | 1 |
| `update` | NETWORK | `update gate-a` | 0 | git pull, 0 REST calls | 4 |
| `sessctl run` | SANDBOX | `sessctl run nosuch` | 1? | D: HTTP to daemon; negative case | 1 |
| `sessctl connect` | SANDBOX | `sessctl connect nosuch` | 1? | D: negative case; a real connect is interactive | 1 |
| `sessctl stop` | SANDBOX | `sessctl stop nosuch` | 1? | D: negative case | 1 |
| `sessctl auth` | SANDBOX | `sessctl auth nosuch` | 1? | D: negative case | 1 |
| `sessctl list` | SANDBOX | `sessctl list` | 0 | D | 1 |
| `meta run` | SKIP | - | - | boots PM plus sub-agents against an LLM; `--demo` canned path not verified | 0 |
| `statusline` | SAFE | `statusline < '{}'` | 0 | stdin JSON | 1 |
| `commit-trailers` | SAFE | `commit-trailers` | 0? | reads local stores | 1 |
| `banner` | SAFE | `banner` | 0 | seeds $H/.trusty-mpm/banner.txt | 1 |
| `mcp add` | SAFE | `mcp add gate-m /bin/echo` | 0 | writes tm-owned CLAUDE_CONFIG_DIR in temp HOME | 1 |
| `mcp share` | SAFE | `mcp share gate-m` | 0 | retired (#7892), prints why | 1 |
| `mcp unshare` | SAFE | `mcp unshare gate-m` | 0 | retired | 1 |
| `mcp remove` | SAFE | `mcp remove gate-m` | 0 | last of the add chain | 1 |
| `mcp list` | SAFE | `mcp list --json` | 0 | ;; 1 |  |
| `mcp get` | SAFE | `mcp get gate-m` | 0 | ;; 1 |  |
| `mcp test` | SAFE | `mcp test gate-m` | 0? | real handshake against /bin/echo (not an MCP server) may fail; use a fixture MCP server | 3 |
| `config keys set` | SKIP | - | - | stores an API key; File-0600 or OS keyring tier | 0 |
| `config keys list` | SAFE | `config keys list` | 0 | names and tiers only; keyring read unverified | 1 |
| `config keys test` | SKIP | - | - | live provider auth probe, needs a key | 0 |
| `config keys unset` | SKIP | - | - | may touch the OS keyring | 0 |
| `wait --for check` | NETWORK | `wait --for check --pr N --repo O/R --timeout 20` | 0 | gh pr checks per poll; 1 to 3 calls; exit 75 means pending | 5 |
| `session <verb>` | SANDBOX | `session list` | 0 | HIDDEN top-level alias of `sessions`, prints a deprecation notice once; one parity smoke | 1 |
| `internal-spawn-disclaimed` | SKIP | - | - | HIDDEN: posix_spawn of argv[0] with TCC disclaimed, runs any program | 0 |
| `sessions managed-stop` | SANDBOX | `sessions managed-stop nosuch` | 1? | HIDDEN deprecated alias of stop; negative case | 1 |
| `sessions runtime-stop` | SANDBOX | `sessions runtime-stop nosuch` | 1? | HIDDEN deprecated alias of stop | 1 |
| `sessions managed-resume` | SANDBOX | `sessions managed-resume nosuch` | 1? | HIDDEN deprecated alias of resume | 1 |
| `secrets __exec-stub` | SKIP | - | - | HIDDEN internal helper of `secrets exec`; never run by hand | 0 |
| `<url> (external subcommand)` | SKIP | - | - | `tm <url>` or `tm <owner/repo>` clones and runs a session (Command::External) | 0 |

## Runtime estimate

| Part | Seconds |
|---|---|
| SAFE rows (76) | 67 |
| SANDBOX rows (71) | 82 |
| NETWORK rows (12) | 52 |
| Command time, serial | 201 |
| Sandbox start-up (about 5) and teardown (about 15: `tmux kill-server`, pid sweep, survivor scan) | 20 |
| Serial total | about 221 s, 3.7 min |

Estimates: about 0.3 s for a read verb (the binary is 89 MB), 4 s for `install` and `reinstall`, 8 s for `doctor` (each probe has its own timeout), 4 to 8 s per NETWORK row.
Add the one-time cost of fetching the published-content fixture, which this inventory does not price.

Parallel lanes (wall time about 60 to 100 s):

- Lane A, SAFE. Independent once each lane has its own `$H`. `install` must precede `agent`, `optimizer`, `content status`, `validate`; `services init` precedes `services list|port|url|log`; `mcp add` precedes `get|list|remove`; `dbatg init` precedes the other `dbatg` verbs; `register` precedes `path` and `rm`. Four lanes cut about 67 s to about 20 s.
- Lane B, SANDBOX. One daemon. The `projects` chain (register, show, status, config, deliverables, milestones, decommission) is serial. The 26 negative cases and the read verbs are independent and can run 4-way. `decommission-ephemeral`, `prune-idle`, `clean`, `prune` mutate the shared registry: run them last.
- Lane C, NETWORK. Serial, so the call count stays bounded and the rate limit is predictable. Can overlap with A and B.

### GitHub API budget (NETWORK lane, per run)

| Command | REST/GraphQL calls | Other traffic |
|---|---|---|
| `content update` | 2 (`git/matching-refs` listing, `releases/tags/{tag}`) | 3 asset downloads from github.com |
| `issue current` | 1 | |
| `issue audit` | 2 to 4 | |
| `issue board init` / `apply` / `check` | 1 to 3 / 2 to 4 / 2 to 4 | |
| `pr queue-check <PR>` | 3 | without `<PR>`: 1 list plus 3 per open PR |
| `wait --for check` | 1 to 3 | |
| `catalog sync`, `catalog apply`, `load`, `update` | 0 | git clone or pull over HTTPS |
| Total | about 14 to 23 | |

Counts come from reading the call sites (`content/bundle_cache.rs:554-565`, `core/issue_audit_gh.rs:81,106`, `issue/board/client.rs:242,513`, `pr/check_gate.rs:190`, `pr/queue_check.rs:298`), not from a trace. Authenticated, 5,000 per hour makes this negligible. Unauthenticated, REST allows 60 per hour per IP, so 23 calls is 38% of the hour, and a second run or a shared CI runner IP breaks it.
Under `env -i` there is no `GH_TOKEN` and no `gh` config in the temp HOME, and `gh` is not on the stated `PATH`. Every `gh`-based row therefore fails unless a token is passed in on purpose. Only `content update` can run unauthenticated (it falls back to unauthenticated reads, `content/release_source.rs:121-180`).

## Risks

1. A clean HOME does not isolate these (each is tm behaviour I read, none run):
   - tmux: the default socket is `/tmp/tmux-<uid>/default`, keyed to the user. `architect poll` and `input-state` read tmux through `TMUX_SOCKET`; unset, they hit the live server.
   - Host process table: `tm stop` and `restart` walk it with `sysinfo` and SIGTERM then SIGKILL every `tm daemon` (`commands/daemon.rs:377-440`); `tm telegram stop` runs `pkill -f "telegram start"`. Neither is scoped by HOME or checked by the host-state gate. Running either in the gate kills the owner's live daemon. Both are SKIP.
   - macOS Keychain: `auth`, `login`, `secrets`, `env set`, `config keys set|unset`, `presence`.
   - `launchctl list`: `tm doctor` runs it read-only.
   - Host ports: `services status|health` probe manifest ports on the host.
2. The brief's env (`HOME`, `PATH`, `TERM`) alone does not set `TRUSTY_SANDBOX=1` or `TRUSTY_DATA_DIR_OVERRIDE`. Without them, `tm start` and the bare-`tm` autostart spawn a plain daemon in the temp HOME, which outlives the gate (`core/discovery.rs:341-352`), and the console probe falls back to the default console address (`core/discovery.rs:481-495`). The sandbox script pins those names for this reason. The gate environment should add them, TMUX_TMPDIR, and `TRUSTY_MPM_SOCKET`. This departs from the literal env in the brief.
3. "Any error fails the release" needs an expected exit per row. 26 SANDBOX rows are negative by design and exit non-zero. 52 of the 159 non-SKIP rows carry an unverified `?` exit. Without a calibration run the first gate run will report false failures.
4. `tm doctor` exits non-zero on any FAIL row. A clean HOME fails by design (no content, no daemon). Rows that read host facts (launchd plist class, trusty-events bus, gcloud under `--network`) can flip with the host. Run it only after `install`, with the sandbox daemon, and without `--network`.
5. Coverage is not "all commands". 78 of 237 rows (33%) are SKIP, and they include the paths with the most behaviour: `sessions start|new`, `launch`, `connect`, `run`, `fleet init`, `architect up`, all of `secrets`, all of `memory`, all `pr`/`issue` writers, `stop`/`restart`. The session lifecycle is tested only through negative cases, because a positive case needs a real `claude`.
6. Drift. A `--help` walk misses hidden commands (6 found) and `tm <url>`. A new command with no row must fail the gate. `generate/cli_tree.rs` renders the clap tree in process; I did not check whether it includes hidden commands.
7. `scripts/sandbox_daemon.sh` constraints (read, not run): it runs in the foreground, so the gate must background it and wait on the socket; `--dir` must keep the socket path under 103 bytes; it refuses when a `.env.local` sits at or above `<dir>/home`; it exits 1 when teardown leaves a process under `<dir>`; a build artifact older than its sources refuses (`--allow-stale`), an installed binary only prints a notice; it reads the real home from `dscl`.
8. The `tm` on this machine's `PATH` is a broken mise shim. The gate must pin an absolute binary path and record `tm --version`.
9. The `?` rows and the "after X" ordering were inferred. A wrong dependency shows as a false red, not a false green.

## Open questions

1. May a one-time calibration run, inside a throwaway sandbox, pin the 52 unverified exit codes and the exact flags marked "per --help"? That run executes commands, which this task forbids.
2. Does the owner accept SKIP-with-reason as "all commands", or must the 78 SKIP rows get a sandbox (a trusty-memory sandbox and a trusty-secrets sandbox script do not exist for `memory` and `secrets`; #9793 covers secrets plus console)?
3. May the gate environment add `TRUSTY_SANDBOX=1`, `TRUSTY_DATA_DIR_OVERRIDE`, `TMUX_TMPDIR`, `TMUX_SOCKET`, `TRUSTY_MPM_SOCKET` to the literal `env -i HOME PATH TERM`?
4. NETWORK: pass a read-only token and fixed target issue and PR numbers, or drop NETWORK rows from the release gate and keep `content update` only?
5. Which bundle is the "published content" fixture for `content install --from`, and is `content update` (2 calls plus downloads) expected to pass unauthenticated?
6. Do negative cases (unknown id exits 1) count as passing when the exit equals the expected code?
7. Are stdin-protocol smokes wanted for `serve --stdio` and `coordinator serve` (one `initialize` request, expect a reply)? `serve` is SKIP now.
8. Where does the gate run, the Mac (launchd, Keychain, macOS-gated tests) or EVO (CLAUDE.md prefers EVO for builds and containers)? tmux, Keychain and `launchctl` behave differently on Linux.
9. Should drift be caught by a test that compares the in-process clap tree (hidden included) with this table, and who owns the table?

End 2026-11-18T16:17:42Z.
