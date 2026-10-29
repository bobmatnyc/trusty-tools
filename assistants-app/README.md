# assistants-app (Python prototype)

Desktop assistants app prototype. All inference goes through AWS Bedrock.
Product brief: `docs/briefs/2026-10-29-tm-agents-assistants-app.md`;
slice plan: `docs/briefs/2026-10-29-tm-agents-epic-proposal.md`.

## Requirements

- Python 3.12 or later
- AWS credentials for profile `cto`, region `us-east-1`, with Bedrock
  access to Claude Sonnet 5.5 (`anthropic.claude-sonnet-5-5`)

## Setup

```bash
python3.12 -m venv assistants-app/.venv
assistants-app/.venv/bin/python -m pip install -r assistants-app/requirements.txt
```

## AWS configuration

The smoke test uses the existing `cto` profile in `~/.aws/config`. Defaults
can be overridden with environment variables:

| Variable | Default |
|---|---|
| `AWS_PROFILE` | `cto` |
| `AWS_REGION` | `us-east-1` |
| `BEDROCK_MODEL_ID` | `us.anthropic.claude-sonnet-5-5` |

Bedrock rejects the bare model ID `anthropic.claude-sonnet-5-5` for
on-demand calls ("on-demand throughput isn't supported"). Calls must use an
inference profile: `us.anthropic.claude-sonnet-5-5` (US regions) or
`global.anthropic.claude-sonnet-5-5`.

If the SSO session has expired, run `aws sso login --profile cto` first.

## Smoke test

```bash
assistants-app/.venv/bin/python assistants-app/smoke_test.py
```

It streams a reply to "Hello, how can you help me today?" and ends with:

```
Input tokens: X, Output tokens: Y, Estimated cost: $Z
```

Cost uses Sonnet 5.5 pricing: $3 per million input tokens, $15 per million
output tokens. A non-zero exit means the Bedrock call failed; the error is on
stderr.
