# 0042. MCP configuration is static and persistent — the declaration lives once in user scope, and nothing injects it into a workspace

- **Status:** Accepted
- **Date:** 2026-08-10 (accepted 2026-08-11)
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

### The bridge's stated rationale is stale for the daemon path, and live everywhere else

[`native_mcp.rs:1-15`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/native_mcp.rs#L1-L15) justifies the whole bridge on one premise: daemon-managed sessions launch `claude --setting-sources project,local`, "which excludes the `user` tier where that map lives, so the managed servers are never read." That premise is real, and it was measured rather than assumed (see the flag table below). What changed is only its reach.

For the **daemon-managed** path it no longer holds. Since #4451 relocated `CLAUDE_CONFIG_DIR`, a relocated spawn uses [`SETTING_SOURCES_FLAG_RELOCATED`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L111) — `--setting-sources user,project,local` — selected by [`setting_sources_flag`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L125), and every daemon-managed spawn relocates. The doc comment was never updated, so on that path the bridge is load-bearing only by inertia.

For the **interactive** paths it still holds exactly as written, which is what makes it a constraint on this decision rather than a historical note. See "The interactive launch paths do not load user scope" in Consequences.

### The measurement that settled the shape

Measured live 2026-08-10 in a daemon-managed session on `bobmatnyc/pm-workflow-test`, a repo with no upstream `.mcp.json`, same session and same spawn: the servers declared only in the shared user-scope `.claude.json` came up `✔ Connected` with no approval, including one absent from the worktree entirely. The four `trusty-*` servers written into the worktree `.mcp.json` came up `⏸ Pending approval (run claude to approve)`.

The live config dir on this machine confirms the split is exactly along file boundaries, not server identity:

```
$ python3 -c "import json; print(sorted(json.load(open(D+'/.claude.json'))['mcpServers']))"
['apex', 'claude_design', 'duetto-code-intelligence', 'duetto-memory', 'duetto-product-intelligence']

$ python3 -c "import json; print(sorted(json.load(open(D+'/.mcp.json'))['mcpServers']))"
['trusty-memory', 'trusty-mpm', 'trusty-review', 'trusty-search']
```

`D = ~/.trusty-tools/trusty-mpm/claude-config`. The five that connect silently are precisely the top-level `mcpServers` map of `<CLAUDE_CONFIG_DIR>/.claude.json` — the map `tm mcp add` writes ([`mcp_config.rs:639`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L639)). The four that require approval are in `<CLAUDE_CONFIG_DIR>/.mcp.json`, written by [`ensure_mcp_config`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/standalone/global_config.rs#L316); those four reach the session through the workspace copy instead.

So workspace-scope declaration is what creates the approval gate. It is also what creates the per-worktree drift, the write-after-spawn race, and the dirty-tracked-file leak the issue describes. One cause, four symptoms.

### How Claude Code discovers `.mcp.json`, measured

Measured 2026-08-10 against `claude` 2.1.226, using a stub stdio MCP server that records its own `argv`, `cwd`, and environment to a file and then completes a real handshake, so a spawn is observed rather than inferred. Every run used a throwaway `CLAUDE_CONFIG_DIR` and a throwaway cwd; no operator configuration was read or written.

**`.mcp.json` is discovered by walking up the directory tree from the session's cwd.** A run whose cwd was an empty scratch directory and whose config dir was freshly created still listed eight servers — exactly the `mcpServers` map of `/private/tmp/.mcp.json`, an ancestor of that cwd.

That settles what `<CLAUDE_CONFIG_DIR>/.mcp.json` is for. The same file, declaring the same server, read twice with only cwd changed:

| cwd | is the config dir's `.mcp.json` server listed? |
|---|---|
| a sibling scratch directory | no — absent entirely |
| the config dir itself | yes, as `⏸ Pending approval` — i.e. as *project* scope |

The file has no special status and no dedicated reader. It is read only when it happens to be an ancestor of cwd, which for a real session — cwd is the repo — it never is. The copy `ensure_mcp_config` writes is inert, and the function can be deleted outright rather than redirected.

The tree walk also means a `.mcp.json` **above** a workspace is read by every session beneath it. `/private/tmp/.mcp.json` is doing that today on this machine: mode `0644`, dated 2026-08-08, declaring all four `trusty-*` servers plus four operator servers, and pinning `trusty-search serve --index tmp`. It is the same drift #4181 describes, one directory further up than the issue assumed, and deleting the injectors does not remove files already written. See Open questions.

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

5. **`tm mcp` does not wrap `claude mcp`.** No such wrap exists today, and none is added. The two CLIs were compared directly and tm's is a strict superset on both operations that matter for seeding:

   ```
   $ claude mcp add adrtest-probe /bin/echo -- hi ; echo "EXIT=$?"
   Added stdio MCP server adrtest-probe with command: /bin/echo hi to local config
   EXIT=0
   $ claude mcp add adrtest-probe /bin/echo -- hi ; echo "EXIT=$?"
   MCP server adrtest-probe already exists in local config
   EXIT=1

   $ tm mcp add --root <throwaway> adr42-idem /bin/echo -- hi ; echo "EXIT=$?"
   Added MCP server 'adr42-idem' to <throwaway>/claude-config/.claude.json
   EXIT=0
   $ tm mcp add --root <throwaway> adr42-idem /bin/echo -- hi ; echo "EXIT=$?"
   MCP server 'adr42-idem' already present (no change)
   EXIT=0
   ```

   `claude mcp add` fails on re-add at the same name and scope; [`add_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L87) succeeds, because [`mcp_config::add_server`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L141) reports whether anything changed instead of erroring. `claude mcp list --help` and `claude mcp get --help` each expose exactly one option, `-h, --help`; tm's [`list_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L176) and [`get_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/mcp.rs#L215) take `--json` and emit it.

   Adding the wrap would put a second writer on the one map this ADR makes authoritative, which the "common entry point" rule in `CLAUDE.md` treats as a defect rather than a convenience. An operator wanting `claude mcp add` can still run it directly — it writes the same file.

6. **The interactive launch paths relocate `CLAUDE_CONFIG_DIR`.** `tm launch`
   and `tm connect` used to spawn `claude --setting-sources project,local` and
   not relocate, which under a user-scope-only declaration leaves them with no
   MCP servers at all. Shipped in #5398 (PR B): both paths now relocate to
   `managed_claude_config_dir()`, pass `--setting-sources user,project,local`,
   call `ensure_managed_config_dir`, and mirror the OAuth token. Open question 1
   is resolved with it — see Q4 below.

## Consequences

### What is deleted

Derived by reading the current tip (`364aba4`), not from any prior report:

- The five injectors in the Context table, plus their shared writer [`inject_mcp_server`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L394) and `native_mcp.rs`'s secret-splitting / `.env.local` routing, which exists only because the file it wrote into was git-tracked.
- `custom_mcp.rs`'s project-scope `[mcp.custom]` injection loop, and with it the `project_scope_mcp_names` subtraction at the [`mod.rs:1146`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/mod.rs#L1146) call site.
- [`exclude_mcp_json_from_git`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/native_mcp.rs#L564), called at [`mod.rs:999`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/mod.rs#L999) and again from `runtime/claude_code.rs`. It is a mitigation for the leak that only exists because tm writes the file; with no write there is nothing to exclude.
- [`launch_trusted_mcp_names`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/mcp_config.rs#L564) / `_from`, [`preseed_workspace_trust`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L697) / `_home`, and the `enabledMcpjsonServers` derivation in `standalone::trust_seed`. **These must go in the same change as the injectors — see the security section below.**
- [`ensure_mcp_config`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/standalone/global_config.rs#L316) is deleted rather than redirected. The measured tree-walk above shows the `.mcp.json` it writes is never read by a session whose cwd is the repo, so there is nothing to preserve.

### Deleting `inject_trusty_memory_mcp` also deletes the #1939 alias healing

`inject_trusty_memory_mcp` is not only an injector. It is the sole call site of [`maybe_register_palace_alias`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/palace_alias.rs#L45), invoked at [`settings.rs:483`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/session_launch/settings.rs#L483) immediately before the write. Nothing else in the crate calls it. Delete the injector as written and #1939's claude-mpm split-brain healing — aliasing `owner-repo` to a pre-existing bare-repo palace — silently stops running, with no test failing to say so. The call has to be rehomed onto whatever still runs per launch, most naturally `ensure_managed_config_dir`.

The original worry behind this question turns out not to apply. `palace_alias.rs:48` does bail when `TRUSTY_MEMORY_PALACE` is set, but it reads that through [`palace_override_from_env`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-common/src/palace_id.rs#L67), which is `std::env::var` in the **calling** process — tm itself. The ADR exports `TRUSTY_MEMORY_PALACE` into the spawned `claude` process's environment, not tm's, so the two never meet and alias registration is unaffected. The risk is the lost call site, not the override.

On the trusty-memory side the export does take the highest-precedence branch: `cwd_palace_slug_at` returns the override immediately, short-circuiting the pin file and the git derivation below it. That is the intended effect — it is what replaces the `.mcp.json` `env` pin. Whether alias resolution still applies downstream of an overridden slug is not settled here; see Open questions.

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

**Measured precedence confirms this, and shows the approval is the whole hinge.** Two runs, identical except for one key, each declaring the same server name `adr42-collide` in both a workspace `.mcp.json` and the user-scope `mcpServers` map, with a distinct stub binary behind each so the spawn log records which one actually ran:

| `enabledMcpjsonServers` contains the name | `claude mcp get` reports | binary actually spawned |
|---|---|---|
| no | `Scope: User config (available in all your projects)` | the user-scope one |
| yes | `Scope: Project config (shared via .mcp.json)` | **the repo's one** |

Unapproved, the repo's colliding entry is inert — it does not shadow, and it does not even surface as pending; only non-colliding repo names appear as `⏸ Pending approval`. Approved, project scope wins outright and the user-scope declaration is overridden.

So the name-squatting attack is not merely *enabled* by the approval, it is *constituted* by it: `enabledMcpjsonServers` is precisely what lets repo content displace an operator's own declaration. Today's force-overwrite injectors defuse it by rewriting the entry to the canonical command before the approval is computed. Removing the approval defuses it by removing the displacement mechanism. Both are coherent; only the half-done state is not.

The property becomes structural rather than procedural: today safety depends on an injector *succeeding* in the same run as the approval; afterwards there is no approval to get wrong. That is a stronger invariant, but only if the removal is atomic. **An implementation that lands the injector deletion without the approval deletion, or in a separate PR, reintroduces #3926 in its worst form.** The implementing PR must carry a regression test asserting that a workspace whose `.mcp.json` declares a builtin name produces no `enabledMcpjsonServers` entry for it — the natural successor to `tests_launch_trust_3926.rs`, which should be rewritten rather than deleted.

Residual risk this accepts: user-scope `mcpServers` entries connect with no approval prompt, so `tm mcp add` becomes the single operator trust decision for MCP. That is already true today for the five non-trusty servers on this machine, and it is operator-initiated by construction — a cloned repo cannot reach that file.

### `trusty-search` needed no change — the precondition this ADR asserted was false

This section originally claimed a `trusty-search` change was a precondition of
the deletion: `trusty_search_mcp_value` baked the index pin as a positional
argument, `Serve` declared its own `index` field with no `env`, and the `Serve`
arm never consulted `cli.index`, so `TRUSTY_INDEX` was "already parsed and
already ignored by exactly the one code path that needs it."

**That was wrong at implementation time.** `trusty-search serve` already honours
`TRUSTY_INDEX` (#5394, PR A), so no per-project pin work was needed in
`trusty-search` at all and the cross-crate rung-4 gate this section demanded does
not apply. What remained was entirely on the trusty-mpm side: exporting the
variable. `core::mcp_session_env::session_mcp_env` builds it from
`register_project_index`'s confirmed id, gated on the same `[mcp] trusty_search`
toggle that used to gate the injector, and every spawn path emits it —
`env_bin_prefix` for the tmux panes, `build_claude_command_with` for
`tm launch` / `tm connect`, and `InPlaceResumeCommand.mcp_env` for the bare-`tm`
in-pane relaunch. `TRUSTY_MEMORY_PALACE` rides the same carrier, derived from
`session_launch::resolve_palace_slug` — the function that outlived the memory
injector it was written for.

#1373's wrong-index regression and #1605's wrong-palace regression are therefore
preserved by an environment variable rather than an injected argument, which is
what item 4 of the Decision proposed.

### The environment carrier works — measured end to end

The whole argless-declaration plan rests on the `claude` process's environment reaching the stdio MCP servers it spawns. It does, unmodified.

An argless server declared only in the user-scope `mcpServers` map came up `✔ Connected`, and the stub recorded a child environment containing every variable set on the `claude` process verbatim — including the three the plan depends on:

```
ADR42_PROBE_MARKER       = adr42-marker-9f3c1e7b
TRUSTY_MEMORY_PALACE     = adr42-palace-probe
TRUSTY_SEARCH_INDEX      = adr42-index-probe
TRUSTY_INDEX             = adr42-trustyindex-probe
```

The child's environment was the parent's plus Claude Code's own additions (`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PROJECT_DIR`, `AI_AGENT`). Nothing was dropped, renamed, or filtered — there is no allowlist and no sanitizing, so a variable name tm chooses freely will arrive. Item 4 of the Decision is sound, and the pins need no other carrier.

One consequence worth naming: because inheritance is plain and unfiltered, the same mechanism carries *any* variable in the spawn's environment into every MCP server the session runs. That is what makes the argless plan work, and it is also why `env_bin_prefix`'s existing marker scrub stays load-bearing.

### The interactive launch paths do not load user scope — this changes the plan

This is the one open question that resolved **against** the design as first drafted, and it is a precondition rather than a caveat.

`--setting-sources project,local` does not merely exclude the `user` *settings* tier. It suppresses the user-scope `mcpServers` map itself. Same throwaway config dir, same declaration, only the flag varying:

| flag on `claude … mcp list` | user-scope server visible? |
|---|---|
| *(none)* | ✔ Connected |
| `--setting-sources project,local` | **not listed at all** |
| `--setting-sources user,project,local` | ✔ Connected |

`CLAUDE_CONFIG_DIR` was set in all three runs, so relocation alone does not rescue it — the flag decides.

Two live paths emit that flag and never relocate:

- `tm launch` builds its pane command through [`build_claude_command`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L256) at [`launch.rs:326`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/launch.rs#L326).
- `tm connect` does the same through [`connect_claude_cmd`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/bin/tm/commands/launch.rs#L622).

`build_claude_command` appends [`SETTING_SOURCES_FLAG`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L86) unconditionally at [`model_inject.rs:273`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L273) — it takes no `config_dir` argument, so `setting_sources_flag`'s relocated branch is unreachable from it. The behaviour is pinned by [`claude_command_includes_setting_sources`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/core/model_inject.rs#L550), which asserts the composed line contains `--setting-sources project,local` and does not contain `user` at all. `prepare_managed_config` ([`claude_code.rs:714`](https://github.com/bobmatnyc/trusty-tools/blob/364aba420214d8191408e54b024bcc5874a2d66a/crates/trusty-mpm/src/runtime/claude_code.rs#L714)) also returns `None` when the home directory cannot be resolved, sending the daemon path down the same non-relocated branch.

Today those sessions still get MCP servers, because the injectors write them into the workspace at project scope — the tier the flag *does* load. **Delete the injectors without touching these paths and `tm launch` and `tm connect` get no MCP servers whatsoever**: nothing at user scope is read, and nothing is written at project scope any more. That is a worse failure than the one #4181 reports, because it is total rather than partial.

Two remedies, both small, neither yet chosen:

- Give `build_claude_command` the same `config_dir` parameter its daemon-side siblings take, relocate on these paths, and let `setting_sources_flag` pick `user,project,local`. This is the uniform answer and it makes the tier contract single-sourced.
- Keep the paths non-relocated and change the flag to `user,project,local` outright. Smaller diff, but it re-admits the operator's global `~/.claude` settings that #1269 excluded on purpose, since without relocation the `user` tier resolves to the operator's own home.

The first preserves #1269's isolation goal by the same mechanism #4451 used; the second trades it away. The choice belongs with the owner, and it is a `trusty-mpm` behaviour change requiring its own regression coverage — the existing test above encodes the current invariant and would have to be rewritten deliberately, not deleted.

The two halves of this finding were established differently, and the distinction matters for anyone rechecking it. That the flag suppresses user scope was **measured** against `claude` 2.1.226. That `tm launch` and `tm connect` emit it was established by **reading** the command builders and the test that pins them; neither command was run end to end, because doing so spawns a real interactive session.

### Smaller consequences

- `#4181`'s two independent complaints separate cleanly. The placement/drift/leak half is resolved by construction. The **second, separable defect the issue names — that a declared-but-missing server is reported nowhere the session can see it — is untouched by this ADR** and stays open.
- Sessions gain a working MCP surface in projects other than `trusty-tools` on first launch, without an approval prompt, which is the symptom that blocks the owner.
- Losing per-project `.mcp.json` means losing per-project MCP *variation*. Any future need for a project to declare a server the operator has not registered must go through Claude Code's own consent dialog, deliberately.

## Open questions

The six questions this ADR opened were investigated on 2026-08-10; five resolved and moved into Context, the Decision, or Consequences above. Q4 (which remedy the interactive launch paths take) resolved at implementation and is recorded below. What is left is genuinely unsettled.

**Q4, resolved (#5398).** The interactive paths RELOCATE `CLAUDE_CONFIG_DIR` and
let `setting_sources_flag` pick `user,project,local` — the first of the two
candidate remedies. The rejected alternative was adding `user` to
`--setting-sources` while still reading the operator's real `~/.claude`, which
re-admits the global settings tier #1269 excluded on purpose. Relocation
preserves that isolation by the same mechanism #4451 used on the daemon path, so
the tier contract stays single-sourced through `setting_sources_flag` rather than
forking into two.

1. **Whether palace-alias resolution still applies to an overridden slug.** `cwd_palace_slug_at` returns a set `TRUSTY_MEMORY_PALACE` immediately, ahead of the pin file and the git derivation. Whether `PalaceAliasStore` resolution then still runs against that slug downstream — which decides if an aliased palace keeps resolving once the pin moves from the `.mcp.json` `env` block to a spawn variable — was not traced to a conclusion. It affects #1939 parity, not the declaration model.

2. **What to do about `.mcp.json` files already written above a workspace.** Discovery walks up from cwd, so a stale ancestor file keeps contaminating sessions after the injectors are gone; `/private/tmp/.mcp.json` is a live example on this machine. Deleting the write path does not delete what it already wrote, and nothing here cleans up. The implementation deliberately keeps `mcp_provenance`'s READ side and `tm doctor`'s stray sweep — only `record_write_best_effort`'s call site went with the injectors — so a follow-up has a ledger to work from. Whether that becomes a `tm doctor` check, a one-shot cleanup, or nothing at all is unaddressed.

A related note, not a question: the same tree walk means any directory above a workspace is a place a `.mcp.json` can be planted. On a shared machine `/tmp` is the obvious one. The user-scope declaration this ADR adopts is unaffected — it is not reached by the walk — but the residual risk in the security section covers only cloned repos, and this widens the surface it should be read against.

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
- All source citations above are pinned to `364aba420214d8191408e54b024bcc5874a2d66a`, the `origin/main` tip at the time of writing; the code they name is deleted by the implementation, so they read as history.
- Implemented in three PRs: #5394 (A — `trusty-search serve` honours `TRUSTY_INDEX`), #5398 (B — the interactive paths relocate `CLAUDE_CONFIG_DIR`), #5406 (C1 — `seed_builtin_servers` seeds user scope), and the deletion itself (C2), which removes the injectors and the `enabledMcpjsonServers` approval together.
- Measurements dated 2026-08-10 were run against `claude` 2.1.226 using a stub stdio MCP server that recorded its `argv`, `cwd`, and environment before completing a handshake, so each result reflects an observed spawn. Every run used a throwaway `CLAUDE_CONFIG_DIR` and a throwaway cwd; the operator's configuration was neither read nor written, and no daemon was restarted. What each one settled is stated where it is used: `.mcp.json` discovery in Context; precedence and environment inheritance in Consequences; the `--setting-sources` behaviour under "The interactive launch paths do not load user scope".
- Palace-alias healing whose only call site the deletion removes: #1939.
