Added
- Model resolution recognizes the Fable tier (`fable` alias, `[models.tiers] fable`, default `claude-fable-5-2`) and reserves it for PM sessions and engineer and research roles. Any other agent that asks for Fable, through `--model`, `[models.agents]`, frontmatter or `[models] default`, runs on Opus, and the substitution is logged with the agent, requested and resolved models. See #11353.
