# Guard policies

Generated from `crates/trusty-mpm/src/core/guard_policy_entries.rs` by the `policy_doc_matches_table` test (#10641). Do not edit by hand; run `TM_UPDATE_GUARD_POLICY_DOC=1 cargo test -p trusty-mpm --lib policy_doc_matches_table`.

Every `tm hook --pm-guard` and `tm hook --egress-guard` refusal ends with `[policy <id> <rule>; source: <source>]`.

| Id | Rule | Binds | Absolute | Architect core limit | Source | Summary |
|---|---|---|---|---|---|---|
| GP-000 | `policy-lookup-miss` | PM, Architect, Agent | yes | the guard stays on | #10641 (owner ruling D29) | A refusal named no policy-table entry, so the guard denies rather than allows. |
| GP-001 | `unreadable-payload` | PM, Architect, Agent | yes | the guard stays on | #7975 | A PreToolUse payload that is empty, unreadable or not a JSON object is denied. |
| GP-002 | `unclassifiable-payload` | PM, Architect, Agent | no | the guard stays on | #7975 | A payload without a string tool_name and an object tool_input is denied. |
| GP-003 | `guard-failure` | PM, Architect, Agent | yes | the guard stays on | #10600, #10641 | A guard run that panics or errors before deciding denies the call. |
| GP-004 | `unclassifiable-command` | PM, Architect, Agent | yes | the guard stays on | #6660, #9001 | A Bash command the guard cannot lex (ANSI-C quoting, unbalanced quotes) is denied. |
| GP-005 | `destructive-delete` | PM, Architect, Agent | yes | the guard stays on | #4031 | No delete of a filesystem root, home, the repo root, a .git directory or a worktree root. |
| GP-006 | `trust-anchor-write` | PM, Architect, Agent | yes | the guard stays on | #8878 D4 | No write to a guard trust anchor (launch records, user config); the Architect has a narrow exemption. |
| GP-007 | `secret-file-read` | PM, Architect, Agent | yes | secret locations only | #7266, #8879, #8939 | No command or Read that prints a secret file or a credential value. |
| GP-008 | `kuzu-store-read` | PM, Architect, Agent | yes | secret locations only | #8625 (ruling B10) | No direct read of a kuzu memory store. |
| GP-009 | `kuzu-import-python` | PM, Architect, Agent | yes | secret locations only | #9794 (ruling Q1) | No trusty-memory kuzu import with --python. |
| GP-010 | `d4-floor` | PM, Agent | yes | - | #8878 D4 | No exfiltration upload, raw disk tool, force-push or anchor write outside the Architect. |
| GP-011 | `architect-pane` | PM, Agent | yes | - | #8902 | No tmux verb aimed at the Architect's pane from any other session. |
| GP-012 | `tmux-target-unresolved` | PM, Architect, Agent | yes | exact pane targets | #9001 | A tmux target must resolve exactly to an existing pane, window or session. |
| GP-013 | `fleet-relaunch` | PM, Architect, Agent | yes | checkpoint first | #10033 | No hooked session runs tm fleet relaunch, the Architect included. |
| GP-014 | `read-only-dispatch` | Agent | no | - | #8439 | A read-only dispatch runs only allowlisted read shapes. |
| GP-015 | `worktree-add` | PM, Architect, Agent | no | the guard stays on | #3977, #7497 | No git worktree add under a temp root or past the disk threshold. |
| GP-016 | `secret-file-copy` | PM, Architect, Agent | no | secret locations only | #7122 | No shell copy or move of a secret-shaped file into a worktree. |
| GP-017 | `main-checkout-destructive` | PM, Agent | no | - | ADR-0037 | No command that destroys uncommitted work in a main checkout. |
| GP-018 | `head-switch` | PM, Architect, Agent | no | the guard stays on | #8572 | No HEAD switch in a main checkout over another session's uncommitted edit. |
| GP-019 | `main-checkout-commit` | PM, Agent | no | - | ADR-0044, ADR-0048, ADR-0049 | No commit in a main checkout beyond a docs-and-config set. |
| GP-020 | `docs-commit` | PM, Agent | no | - | ADR-0049 | No docs commit in a main checkout another live writer stands in. |
| GP-021 | `head-move` | PM, Agent | no | - | ADR-0048 decision 10, ADR-0053, #9127 | No merge or rebase of a main checkout another live writer stands in. |
| GP-022 | `linked-worktree-head-move` | PM, Agent | no | - | #8161 | No HEAD move into a linked worktree a live agent holds. |
| GP-023 | `main-checkout-write` | PM, Agent | no | - | ADR-0044, ADR-0048 | No edit-tool write into a main checkout outside the write boundary. |
| GP-024 | `protected-path-write` | PM, Agent | no | - | #8928 | No write to settings, .mcp.json, git config or hooks, or hook-run paths. |
| GP-025 | `subagent-fanout` | Agent | no | - | #4784 | A subagent never dispatches further subagents. |
| GP-026 | `enter-worktree` | Agent | no | - | #7172 | A worktree-pinned agent never switches worktrees. |
| GP-027 | `worktree-remove` | Agent | no | - | #5791, ADR-0057 | An agent never removes a worktree; the PM does. |
| GP-028 | `worktree-remove-recheck` | Agent | no | - | ADR-0057, #7889 | A version-control removal proceeds only once clean, pushed, merged and unowned are proven. |
| GP-029 | `resume-worktree` | PM, Architect | no | the guard stays on | #8004 | No SendMessage that resumes an agent whose isolated worktree is gone. |
| GP-030 | `dispatch-content` | PM, Architect | no | the guard stays on | #9011 (owner ruling 09(a)) | A dispatch is classified against the installed content roster; with none it is refused. |
| GP-031 | `worktree-grant` | PM, Architect | no | the guard stays on | ADR-0048 | A writer dispatched from a main checkout gets a worktree or is refused. |
| GP-032 | `worktree-grant-dispatch` | PM, Architect | no | the guard stays on | ADR-0048, #5769 | A granted dispatch is refused while another live writer holds the checkout. |
| GP-033 | `dispatch` | PM, Architect | no | the guard stays on | #4480, #5923 | No second file-mutating agent in a working directory that already has one. |
| GP-034 | `agent-cost` | Agent | no | - | #4837 | A subagent past its context ceiling is stopped. |
| GP-035 | `build-lease` | PM, Architect, Agent | no | shared target directories | #8261 | A heavy build runs under tm build-lease or not at all. |
| GP-036 | `persona-deny-by-default` | Agent | no | - | #2231 | Opt-in: a subagent whose persona failed is refused guarded tools. |
| GP-037 | `file-change-budget` | PM | no | - | #2918 (prohibitions P1, P5) | A PM's direct file changes are capped per turn; past the cap it delegates. |
| GP-038 | `pm-tool-policy` | PM | no | - | #1977 (prohibitions P1-P11) | A PM delegates builds, tests, network calls and source edits. |
| GP-039 | `egress-unreadable-payload` | PM, Architect, Agent | yes | the guard stays on | #8454 | The egress guard denies a payload it cannot read. |
| GP-040 | `egress-unclassifiable-payload` | PM, Architect, Agent | yes | the guard stays on | #8454 | The egress guard denies a payload it cannot classify. |
| GP-041 | `egress-unnamed-resource-server` | PM, Architect, Agent | yes | the guard stays on | #8454 | An MCP resource call must name its server. |
| GP-042 | `egress-malformed-mcp-name` | PM, Architect, Agent | yes | the guard stays on | #8454 | An MCP tool name the guard cannot parse is denied. |
| GP-043 | `egress-family` | PM, Architect, Agent | yes | no unruled outward action | #8454, #10387 | No call to a messaging or mail family (Slack, Gmail, Notion, chat) outside its allowlist. |
| GP-044 | `egress-bash` | PM, Architect, Agent | yes | no unruled outward action | #8454 | No Bash command that reaches a messaging or mail endpoint. |
| GP-045 | `egress-channel-tainted` | PM, Architect, Agent | yes | no unruled outward action | #8433, #8454 | No outward call from a session whose channel is tainted. |
| GP-046 | `egress-policy-not-ready` | PM, Architect, Agent | yes | no unruled outward action | #9741, #8454 | A tainted call is denied while the egress policy cannot be loaded. |
| GP-047 | `egress-architect-audit-unwritable` | Architect | yes | no unruled outward action | #10447 | An Architect egress allow stands only once its audit line is written. |
| GP-048 | `egress-guard-panic` | PM, Architect, Agent | yes | the guard stays on | #8454 | An egress guard run that panics denies the call. |
