Added

- Beta overlay config pack in `agents/trusty-agents/overlays/beta/`: `beta-assistant` on Opus 5.5 and a Sonnet 5.5 `research-agent`, both pinned to Bedrock, loaded only when `TAGENT_CONFIG_DIR` points at the directory (#10979). Launch recipe: `docs/trusty-agents/user/beta-bedrock-launch.md`. The sub-agent is `research-agent`, not the `beta-researcher` the issue names, because the ADR-0024 delegation floor admits only that name.
