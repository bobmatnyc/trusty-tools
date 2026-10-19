# tm-delegation-patterns — models and cost

## Per-Agent Model Overrides and the Cost Model

This is the default routing (moved here from the instruction package by #7423,
which keeps the one-line mapping). An explicit `model=` in an Agent call always
wins; omitting it is not a fallback to opus — `resolve_agent_model`
(`core/config.rs`) ranks a per-agent `~/.trusty-mpm/config.toml` entry next,
then the agent's own frontmatter default (what the roster's `Model:` line
reports), then the built-in `sonnet` fallback.

| Task Type | Model to pass | Examples |
|-----------|--------------|---------|
| Simple/routine | `model: "haiku"` | Commit, format, read config, docs, lint |
| General work | `model: "sonnet"` | Research, ops, QA, analysis, general tasks |
| Coding/engineering | `model: "opus"` | Implement, refactor, debug, test writing |
| Complex planning | Route to `research` (`model: "sonnet"`) | Architecture, system design, RFC drafting, roadmaps, trade-off analysis |

Standing per-agent defaults are
set in `~/.trusty-mpm/config.toml`, taking priority over built-in defaults and
agent frontmatter but not over an explicit `model=`:

```toml
[models.agents]
engineer = "opus"
research = "sonnet"
```

The tier aliases `haiku` / `sonnet` / `opus` are resolved to concrete models at
dispatch by `expand_model_alias`, reading `[models.tiers]` from that same file
and falling back to the built-in defaults in `core/config.rs`. Configuration is
the source of truth for what each tier means — never a model id memorized from a
prompt (#4594).

Cost rises steeply haiku → sonnet → opus. Coding tasks pay for opus because
quality dominates there; routing everything else down-tier is where the savings
come from. Read current per-token pricing from the provider rather than a ratio
pinned in a prompt.
