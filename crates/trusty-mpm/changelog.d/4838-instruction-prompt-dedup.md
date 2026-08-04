Changed

- Collapsed six agent-routing surfaces in the compiled PM prompt into one Routing Table plus the generated roster. `Agent Routing` (core), `When to Delegate to Each Agent`, `Ops Agent Routing`, `Make / Mise Command Routing` and `Common User Request Routing` all answered "which agent handles what" for the same ~11 agents; every routing fact that existed only in a deleted table — `make`/`mise run` delegation, version/publish → `local-ops`, the browser-tool ban, the "just do it" pipeline — was folded into the survivor and is now pinned by `the_surviving_routing_table_covers_every_folded_mapping`
- Dropped the `Handles:` line from generated Delegation Authority roster entries. Its text was the agent's frontmatter `description`, byte-identical to the description the harness already publishes in its own Agent-type catalog; `Role:` and `Model:`, which the harness does not supply, are kept
- Stated the direct-action budget once, in `enforcement.md`'s canonical "The direct-action budget (P1 and P5 only)". `core.md`, `identity.md` and `non-overridable-rules.md` now point at it by title instead of restating it — the floor stays self-sufficient because `enforcement.md` is itself part of the floor
- Rewrote `BASE-AGENT.md` in a compressed, instruction-dense register for its agent reader: imperative mood, tables over prose, rules stated once, examples kept only where the rule alone is ambiguous. All 139 rule markers verified present before and after; mirrors in `trusty-mpm` and `trusty-code` byte-identical

Added

- Two prose rules in the PM's "Prose Style — Write Plainly", mirrored into `BASE-AGENT.md`: no praise for the user (acknowledge with "OK" or disagree and say why), and delete the framing opener and lead with the fact. Both state the ban as a category or template with their examples explicitly marked non-exhaustive, because a literal example list had already failed to generalize

Fixed

- Agent frontmatter using a YAML block scalar (`description: >` or `|`) is now folded instead of taken literally. Five bundled writing agents — `copyeditor`, `pangram-editor`, `proofreader`, `writer`, `writing-critic` — rendered into the PM prompt as a bare `>`; the same defect would have silently truncated any block-scalar `role:` or `model:`
