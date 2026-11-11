# Beta launch: Opus 5.5 main agent, Sonnet 5.5 sub-agent on Bedrock

How to run `tagent` from a branch with the beta config pack: `beta-assistant`
on Opus 5.5 delegates research to `research-agent` on Sonnet 5.5. Both are
pinned to AWS Bedrock. Issue #10979, epic #10977 (M1-T3).

## Deviation from #10979

The sub-agent is named `research-agent`, not `beta-researcher` as #10979
asks. The ADR-0024 delegation floor
(`agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS`) admits only
`research-agent` and `ticketing-agent` by name. A `beta-researcher` would load
but could never be delegated to. The Architect ruled for this rename (option A)
and left the floor unchanged. #10979's criteria are therefore not met as
written until Bob accepts the rename, or picks option B (a floor change).

## What the pack is

Two files in `content/agents/trusty-agents/overlays/beta/`:

| File | Model | Notes |
|---|---|---|
| `beta-assistant.toml` | `bedrock/us.anthropic.claude-opus-5-5` | `extends = "assistant"`; `max_tokens = 8192`; `[[stores]]` palace `beta-assistant`; reaches only `research-agent` (`replace_subagents = true`) |
| `research-agent.toml` | `bedrock/us.anthropic.claude-sonnet-5-5` | the bundled `research-agent` with routing changed: no `runner = "claude-code"`, Bedrock pin, AWS profile |

Both set `provider_id = "bedrock"`, `aws_profile = "cto"` and
`aws_region = "us-east-1"`. Edit the profile in both files if yours differs.

The directory is outside the bundled roster, so `tagent` never deploys it to
`~/.trusty-agents/agents`. It takes effect only when `TAGENT_CONFIG_DIR` points
at it. That directory is then searched before `~/.trusty-agents/agents`, so its
`research-agent` shadows the bundled one for that process only.
`beta-assistant` still finds its base `assistant` in the deployed tier.

## Prerequisites

- The AWS profile resolves in the SDK chain, and Opus 5.5 and Sonnet 5.5
  inference profiles are enabled in the region.
- #10939 is on the branch you build. Without it, Bedrock rejects the
  `temperature` field that both agents send.
- For persistence across restarts: a trusty-memory daemon reachable by
  `tagent`. The sandbox script below points the memory socket at a dead path,
  so a sandboxed run does not persist chat history.

## Run from a branch

From the branch checkout:

```bash
cargo build -p trusty-agents --bin tagent
export TAGENT_CONFIG_DIR="$PWD/content/agents/trusty-agents/overlays/beta"
scripts/sandbox_agents_daemon.sh --aws-profile cto
```

The sandbox script forwards `TAGENT_CONFIG_DIR`, passes the AWS profile file
paths (never their contents), and keeps all state under its own directory. Open
the printed URL and select **Beta Assistant**.

To lock every route to Bedrock, also export `TAGENT_PROVIDER_LOCK=bedrock`
once #10978 has landed. Under the lock, an agent whose slug is not
`bedrock/...` fails to load rather than falling back to another provider.

## Live check (not yet run)

1. Start the sandbox as above.
2. Send `beta-assistant` one question that needs research.
3. Confirm one Opus 5.5 call and one Sonnet 5.5 call in the Bedrock logs, or
   in the Costs tab once #10980 lands.
4. With a trusty-memory daemon reachable, restart `tagent --api`, reload the
   chat, and confirm the earlier turns come back.

Loader coverage: `beta_pack_parent_is_opus_on_bedrock_and_reaches_only_the_pinned_child`
and `beta_pack_child_is_sonnet_on_bedrock_and_tracks_the_bundled_research_agent`
in `crates/trusty-agents/src/agents/tests/beta_pack.rs`.
