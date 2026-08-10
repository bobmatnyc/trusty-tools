# 0042. MCP configuration is static and persistent — the declaration lives once in user scope, and nothing injects it into a workspace

- **Status:** Proposed
- **Date:** 2026-08-10
- **Scope:** crate `trusty-mpm` (`core/session_launch/{settings,native_mcp,custom_mcp,search_index,mod}.rs`, `core/mcp_config.rs`, `core/standalone/{global_config,trust_seed}.rs`, `bin/tm/commands/mcp.rs`); crate `trusty-search` (the `serve --index` pin, which is the one declaration that cannot be shared as written)
- **Reversibility Cost:** Medium — the deletion itself is cheap and mechanical, but it removes the mechanism that currently pins MCP *content* behind a pre-approved *name*, so restoring the old shape after the fact means re-deriving the whole #3918 → #3924 → #3926 → #3934 → #3950 chain of security fixes rather than reverting one commit
- **Decision Drivers:** owner ruling 2026-08-10 (verbatim below); issue [#4181](https://github.com/bobmatnyc/trusty-tools/issues/4181), which gates the `tm 1.3.6` milestone and is what currently blocks the owner from using trusty-mpm at all; live measurement showing that workspace-scope declaration is what creates Claude Code's approval gate while user-scope declaration does not; the standing preference for deleting a mechanism over hardening it
- **Supersedes / Superseded by:** — (amends nothing; see Related Decisions)

## Context

### Why this ADR exists now

Issue [#4181](https://github.com/bobmatnyc/trusty-tools/issues/4181) is the blocking item on the `tm 1.3.6` milestone. Its escalation comment is unambiguous: the owner cannot use trusty-mpm until MCP works, several in-flight PRs ride the MCP-fix release, and nothing publishes until he can use it. The issue was filed as a *placement* defect — MCP config written into an ephemeral worktree, when the servers it declares are user-scoped — and its original resolution direction was "move the write to a stable path." The owner's ruling replaces that direction: remove the write.

### What tm does today

On every `prepare_session` run, tm force-writes MCP server entries into the session workspace's `.mcp.json`, then pre-approves those names in the operator's `.claude.json`:

| Injector | Location | Scope of the write |
|---|---|---|
| `inject_trusty_mpm_mcp` | [`settings.rs:531`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L531) | unconditional, no manifest toggle |
| `inject_trusty_review_mcp` | [`settings.rs:559`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L559) | unconditional, no manifest toggle |
| `inject_trusty_memory_mcp` | [`settings.rs:473`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L473) | manifest-toggleable; pins `env.TRUSTY_MEMORY_PALACE` |
| `inject_trusty_search_mcp` | [`search_index.rs:72`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/search_index.rs#L72) | manifest-toggleable; pins `--index <id>` as a positional arg |
| `inject_native_trusty_mcps` | [`native_mcp.rs:166`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/native_mcp.rs#L166) | bridges the operator's `tm mcp add` registry into the workspace, splitting secrets out to `.env.local` |

All five route through one read-merge-write helper, [`inject_mcp_server`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L394). Their success bools then feed [`launch_trusted_mcp_names`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L564) at the [`mod.rs:1146`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/mod.rs#L1146) call site, and the resulting name set is written as `enabledMcpjsonServers` by [`preseed_workspace_trust`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L697).

### The bridge's stated rationale is stale

[`native_mcp.rs:1-15`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/native_mcp.rs#L1-L15) justifies the whole bridge on one premise: daemon-managed sessions launch `claude --setting-sources project,local`, "which excludes the `user` tier where that map lives, so the managed servers are never read." That was true when written (it cites #2756). It is false now. Since #4451 relocated `CLAUDE_CONFIG_DIR`, a relocated spawn uses [`SETTING_SOURCES_FLAG_RELOCATED`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L111) — `--setting-sources user,project,local` — selected by [`setting_sources_flag`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L125), and every daemon-managed spawn relocates. The doc comment was never updated, so the bridge is now load-bearing only by inertia.

### The measurement that settled the shape

Measured live 2026-08-10 in a daemon-managed session on `bobmatnyc/pm-workflow-test`, a repo with no upstream `.mcp.json`, same session and same spawn: the servers declared only in the shared user-scope `.claude.json` came up `✔ Connected` with no approval, including one absent from the worktree entirely. The four `trusty-*` servers written into the worktree `.mcp.json` came up `⏸ Pending approval (run claude to approve)`.

The live config dir on this machine confirms the split is exactly along file boundaries, not server identity:

```
$ python3 -c "import json; print(sorted(json.load(open(D+'/.claude.json'))['mcpServers']))"
['apex', 'claude_design', 'duetto-code-intelligence', 'duetto-memory', 'duetto-product-intelligence']

$ python3 -c "import json; print(sorted(json.load(open(D+'/.mcp.json'))['mcpServers']))"
['trusty-memory', 'trusty-mpm', 'trusty-review', 'trusty-search']
```

`D = ~/.trusty-tools/trusty-mpm/claude-config`. The five that connect silently are precisely the top-level `mcpServers` map of `<CLAUDE_CONFIG_DIR>/.claude.json` — the map `tm mcp add` writes ([`mcp_config.rs:639`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L639)). The four that require approval are in `<CLAUDE_CONFIG_DIR>/.mcp.json`, written by [`ensure_mcp_config`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/standalone/global_config.rs#L316) — a file at a path Claude Code appears not to read for a session whose cwd is elsewhere, since those four reach the session through the workspace copy instead.

So workspace-scope declaration is what creates the approval gate. It is also what creates the per-worktree drift, the write-after-spawn race, and the dirty-tracked-file leak the issue describes. One cause, four symptoms.

## Decision

We adopt the owner's ruling, verbatim across two messages, 2026-08-10:

> *"MCP configuration should be static and persistent"*
>
> *"We should allow changing/modification, but injection shouldn't be needed."*

Concretely:

1. **The declaration lives once, in user scope, and persists.** Its home is the top-level `mcpServers` map of `<CLAUDE_CONFIG_DIR>/.claude.json` — the map `tm mcp add` already writes and the map the measurement above shows connects without approval. The framework builtins are seeded there once, idempotently, from the existing catalog ([`BUILTIN_MANAGED_MCP_SERVERS`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L73), [`builtin_server_entry`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L163)), not on every launch.

2. **Nothing writes MCP config into a workspace `.mcp.json` on launch.** Every injector in the table above is deleted, not relocated.

3. **`tm mcp add` / `remove` / `list` / `get` / `test` stay.** They are the supported way to change the declaration — the "allow changing/modification" half of the ruling.

4. **Declarations become argless where they can be.** A single shared declaration cannot carry a per-project argument, so per-project state moves to environment variables the spawn exports. `trusty-memory` already has the shape (`TRUSTY_MEMORY_PALACE`); `trusty-search` does not (see Consequences).

5. **The `claude mcp` CLI is wrapped for ad hoc operator use only, not adopted wholesale.** Both limitations were verified against the installed CLI at `/Users/masa/.local/bin/claude`:

   ```
   $ claude mcp add adrtest-probe /bin/echo -- hi ; echo "EXIT=$?"
   Added stdio MCP server adrtest-probe with command: /bin/echo hi to local config
   EXIT=0
   $ claude mcp add adrtest-probe /bin/echo -- hi ; echo "EXIT=$?"
   MCP server adrtest-probe already exists in local config
   EXIT=1
   ```

   `claude mcp list --help` and `claude mcp get --help` each list exactly one option, `-h, --help` — there is no `--json`. So `add` is not idempotent (it fails on re-add at the same name and scope) and the read commands have no machine-readable output. tm keeps its own idempotent writer for the one-time seeding and its own `--json`-capable reader ([`list_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L176), [`get_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L215)); the `claude mcp` wrap is a convenience for an operator adding a one-off server by hand.

## Consequences

### What is deleted

Derived by reading the current tip (`364aba4`), not from any prior report:

- The five injectors in the Context table, plus their shared writer [`inject_mcp_server`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L394) and `native_mcp.rs`'s secret-splitting / `.env.local` routing, which exists only because the file it wrote into was git-tracked.
- `custom_mcp.rs`'s project-scope `[mcp.custom]` injection loop, and with it the `project_scope_mcp_names` subtraction at the [`mod.rs:1146`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/mod.rs#L1146) call site.
- [`exclude_mcp_json_from_git`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/native_mcp.rs#L564), called at [`mod.rs:999`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/mod.rs#L999) and again from `runtime/claude_code.rs`. It is a mitigation for the leak that only exists because tm writes the file; with no write there is nothing to exclude.
- [`launch_trusted_mcp_names`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L564) / `_from`, [`preseed_workspace_trust`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L697) / `_home`, and the `enabledMcpjsonServers` derivation in `standalone::trust_seed`. **These must go in the same change as the injectors — see the security section below.**
- [`ensure_mcp_config`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/standalone/global_config.rs#L316)'s target changes from `<CLAUDE_CONFIG_DIR>/.mcp.json` to that dir's `.claude.json` `mcpServers`. The live inspection above shows the `.mcp.json` it writes today is inert.

### What survives

- `tm mcp add` / `remove` / `list` / `get` / `test` and the `.claude.json` writer at [`mcp_config.rs:639`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L639), unchanged. This is the ruling's "allow changing/modification" half and it already works.
- `BUILTIN_MANAGED_MCP_SERVERS` and `builtin_server_entry` — repurposed from a per-launch force-overwrite source into a one-time seeding source.
- [`ensure_managed_config_dir`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/managed_config.rs#L142) and its call from [`claude_code.rs:735`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/runtime/claude_code.rs#L735). It stays as the provisioning hook, and it is where the one-time seeding belongs: it already runs before every spawn, already owns the config dir, and already funnels into `ensure_global_config_dir`. Its non-fatal contract must be preserved — a seeding failure warns and the session still launches.
- `claude_json_guard`'s process-wide lock. tm still writes `.claude.json` for other reasons, and the seeding is one more load-mutate-store cycle on that file.
- [`mcp_server_names`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L498) as a diagnostic reader — it is already documented as never valid for a trust decision, and reporting what a workspace declares stays useful.

### The name-squatting attack, and why removal answers it

This is the part that cannot regress. The current design pins MCP *content* behind an approved *name*. Claude Code's `enabledMcpjsonServers` approval is name-based and content-blind: it reads whatever command sits under that name in the workspace `.mcp.json`. A hostile clone committing a `trusty-mpm` entry pointing at an attacker-controlled binary would have it execute with the operator's credentials on the first `tm session new`, with no human present to decline. The force-overwrite injectors close that by rewriting the entry to the canonical framework command in the same run the approval is computed, and #3950 tightened it further so a name is approved only when its injector's own `Result` proves the write succeeded. This chain — #3918, #3924, #3926, #3934, #3950 — is the most expensive security history in the crate, with a dedicated regression test at `crates/trusty-mpm/src/core/session_launch/tests_launch_trust_3926.rs`.

**Removing the injectors while keeping the approval would be that vulnerability at maximum blast radius.** The answer is that the approval is removed with them, and the attack then has nothing to attach to:

- tm pre-approves no MCP server name at project scope. `enabledMcpjsonServers` is not written at all.
- A name a repo commits into its `.mcp.json` is therefore unapproved, and falls through to Claude Code's own "new MCP servers found" consent dialog — the same gate #2739's `[mcp.custom]` design already relies on for project-scope entries.
- The servers the session actually needs are declared in user scope, where the operator put them, and are never sourced from repo content.

The property becomes structural rather than procedural: today safety depends on an injector *succeeding* in the same run as the approval; afterwards there is no approval to get wrong. That is a stronger invariant, but only if the removal is atomic. **An implementation that lands the injector deletion without the approval deletion, or in a separate PR, reintroduces #3926 in its worst form.** The implementing PR must carry a regression test asserting that a workspace whose `.mcp.json` declares a builtin name produces no `enabledMcpjsonServers` entry for it — the natural successor to `tests_launch_trust_3926.rs`, which should be rewritten rather than deleted.

Residual risk this accepts: user-scope `mcpServers` entries connect with no approval prompt, so `tm mcp add` becomes the single operator trust decision for MCP. That is already true today for the five non-trusty servers on this machine, and it is operator-initiated by construction — a cloned repo cannot reach that file.

### `trusty-search` is the one declaration that cannot be shared as written

`trusty_search_mcp_value` bakes the index pin as a positional argument, `["serve", "--index", "<id>"]` ([`search_index.rs:40-53`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/search_index.rs#L40-L53)), so a single shared declaration cannot carry a per-project value. The fix is smaller than "add an env fallback," because a fallback partly exists already and is shadowed:

- `trusty-search`'s **global** CLI flag already carries one: `#[arg(short = 'i', long, global = true, env = "TRUSTY_INDEX")]` at [`main.rs:49-50`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-search/src/main.rs#L49-L50).
- The `Serve` subcommand declares its **own** `index` field with a bare `#[arg(long)]` and no `env` at [`main.rs:591-592`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-search/src/main.rs#L591-L592), and the `Serve` arm resolves the pin from that field alone, never consulting `cli.index` ([`main.rs:1414-1432`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-search/src/main.rs#L1414-L1432)). Every other subcommand reads `cli.index`.

So `TRUSTY_INDEX` is already parsed and already ignored by exactly the one code path that needs it. Making the `Serve` arm fall back to `cli.index` (or adding `env = "TRUSTY_INDEX"` to the subcommand field) is a small diff, but it is still a `trusty-search` change that trusty-mpm's launch path then depends on — a cross-crate contract, **rung 4** of the test ladder, with `cargo check --workspace` plus `cargo test -p trusty-mpm`. It is not free and it is not optional: without it, the argless `trusty-search` declaration loses index pinning and #1373's wrong-index regression returns.

The environment variable itself must be exported by the spawn. `spawn_command` ([`claude_code.rs:303`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/runtime/claude_code.rs#L303)) exports no per-project MCP variables today — [`env_bin_prefix`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/runtime/claude_code.rs#L197) sets `CLAUDE_CONFIG_DIR` and an OAuth token and scrubs inherited markers, nothing more. `TRUSTY_MEMORY_PALACE` reaches trusty-memory today only through the injected `.mcp.json` `env` block, and `TRUSTY_SEARCH_INDEX` does not exist anywhere in the workspace. Both exports are new work this ADR proposes, not existing behaviour.

### Smaller consequences

- `#4181`'s two independent complaints separate cleanly. The placement/drift/leak half is resolved by construction. The **second, separable defect the issue names — that a declared-but-missing server is reported nowhere the session can see it — is untouched by this ADR** and stays open.
- Sessions gain a working MCP surface in projects other than `trusty-tools` on first launch, without an approval prompt, which is the symptom that blocks the owner.
- Losing per-project `.mcp.json` means losing per-project MCP *variation*. Any future need for a project to declare a server the operator has not registered must go through Claude Code's own consent dialog, deliberately.

## Open questions

Recorded rather than asserted, because they were not verified:

1. **Precedence on a name collision.** If a repo's `.mcp.json` declares `trusty-memory` and the user-scope `.claude.json` also does, which wins, and does the workspace copy still surface a consent prompt? Not tested. This decides whether a hostile repo can shadow a working user-scope server or merely add an unapproved one.
2. **Does an MCP child process inherit the spawn's environment?** The env-var plan assumes the `claude` process's environment reaches the stdio MCP servers it spawns, which is ordinary Unix inheritance, but it was not measured end to end for `TRUSTY_MEMORY_PALACE` under an argless user-scope declaration. If Claude Code sanitizes the child environment, item 4 of the Decision does not work and the pins need another carrier.
3. **What is `<CLAUDE_CONFIG_DIR>/.mcp.json` actually for?** The live inspection shows it holds the four builtins and that they nonetheless require approval when reached via the workspace, which is consistent with the file being unread — but "consistent with" is not "confirmed." Whether `ensure_mcp_config`'s target should be redirected or the function deleted outright depends on that.
4. **The standalone `tm run` driver.** This ADR reasons about the daemon-managed path, where the relocated `CLAUDE_CONFIG_DIR` guarantees the `user` tier is loaded. The non-relocated path still selects `--setting-sources project,local` ([`model_inject.rs:125`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L125)), which is exactly the premise that made the bridge necessary. Whether any live spawn path still takes that branch was not established; if one does, it needs a different answer.
5. **The `trusty-memory` palace pin under a shared declaration.** `inject_trusty_memory_mcp` derives the slug per project from the repo URL. Moving that to `TRUSTY_MEMORY_PALACE` on the spawn is straightforward, but `palace_alias.rs` treats a set `TRUSTY_MEMORY_PALACE` as an *operator override* and bails on aliasing when it sees one — so an unconditionally-exported variable may change alias-resolution behaviour. Not investigated.
6. **Whether `tm mcp` should wrap `claude mcp add` at all.** The two limitations are verified; whether the wrap earns its keep given tm already has a superset writer is a product call, not a technical one.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior ADRs on 2026-08-10:

- **ADR-0014 (Ship full native MCP support):** Consistent — 0014 decides *which* MCP servers ship and where the framework lives; this ADR decides *where their declaration is stored and who writes it*. Different axis, no overlap.
- **ADR-0040 (`trusty-mcp` extracted; `trusty-mcp-services` absorbs `trusty-gworkspace`) and ADR-0041 (`trusty-okg` stays native; agent-facing reads front it as a service):** Consistent — both apply the consumer criterion to decide whether a capability is MCP-shaped at all. This ADR takes the resulting set of MCP servers as given and governs only their declaration lifecycle. A server added by either ADR is declared exactly the same way.
- **ADR-0036 (all worktrees are siblings under `.claude/worktrees/`) and ADR-0037 (PM placement defaults to the main checkout):** Extends — both settle *where a session lives*, and #4181 is the observation that per-worktree MCP config drifts across whatever set of worktrees those decisions produce. Removing per-workspace MCP state makes session placement irrelevant to MCP correctness, which is the direction both ADRs point.
- **ADR-0020 (session-owned worktrees) and ADR-0023 (worktree authority: existence vs ownership):** Consistent — worktree lifecycle and ownership are untouched. This ADR removes a per-worktree *file write*, not a per-worktree anything else.
- **ADR-0018 (loopback-only doctrine), ADR-0031/0032 (UDS inter-crate, console the only HTTP surface), ADR-0034/0035:** Consistent — those govern transport and trust boundaries between running services. MCP stdio declaration is neither. No claim here changes how any daemon binds or is reached.
- **ADR-0026 (a credential grant does not survive delegation):** Consistent, and reinforced. `native_mcp.rs`'s secret-splitting to `.env.local` exists because tm was writing credentials near a git-tracked file; deleting the workspace write removes that pathway entirely rather than continuing to route around it.
- **ADR-0039 (operator-named sessions unique by construction):** Consistent — session naming and slot registry are untouched.
- **ADR-0030 (a session owns many workstreams from the tm checkout):** Consistent — a static user-scope declaration is strictly *more* compatible with one session spanning N workstreams than a per-workspace injection is, since it removes the per-workstream copy the 1:N model would otherwise multiply.

No conflicts found. No prior ADR is superseded or amended.

## References

- Issue [#4181](https://github.com/bobmatnyc/trusty-tools/issues/4181) — the defect, its escalation to the `tm 1.3.6` gate, and the owner ruling recorded as its latest comment.
- Security chain this decision must not regress: #3918, #3924, #3926, #3934, #3950, with the regression test at `crates/trusty-mpm/src/core/session_launch/tests_launch_trust_3926.rs`.
- Consent-gate design for project-scope entries: #2739.
- Index-pin regression this must preserve: #1373.
- `CLAUDE_CONFIG_DIR` relocation that invalidated the bridge's premise: #4451.
- All source citations above are pinned to `364aba420214d8191408e54b024bcc5874a2d66a`, the `origin/main` tip at the time of writing.
