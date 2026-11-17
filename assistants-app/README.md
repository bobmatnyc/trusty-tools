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

## Database

Assistants and messages live in SQLite (`schema.sql`, accessed through
`db.py`). The database file defaults to `assistants-app/assistants.db`; set
`ASSISTANTS_DB_PATH` to use another file.

```bash
assistants-app/.venv/bin/python assistants-app/seed.py
sqlite3 assistants-app/assistants.db "SELECT * FROM assistants"
```

`seed.py` creates the "General" and "Code Helper" assistants. A re-run adds
no duplicates.

## Server

`server.py` is a FastAPI app. Start it from the repository root:

```bash
assistants-app/.venv/bin/uvicorn assistants-app.server:app --reload
```

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/assistants` | Create: `{"name", "instructions", "model"}`; 409 on a duplicate name |
| `GET` | `/assistants` | List all assistants |
| `GET` | `/assistants/{id}` | Get one assistant |
| `POST` | `/assistants/{id}/chat` | Send `{"content": "..."}`; streams the reply |
| `GET` | `/assistants/{id}/messages` | Message history, oldest first |

The chat endpoint sends the assistant's full history to Bedrock
`converse_stream` and streams newline-delimited JSON:

```
{"type": "delta", "text": "Hello! How can I help"}
{"type": "done", "message_id": 2, "tokens_in": 33, "tokens_out": 13, "cost_usd": 0.000294}
```

The user message is stored before the call; the assistant message, with
tokens and cost, is stored when the stream completes. Throttling and quota
errors are retried with exponential backoff (4 attempts) until the first
delta is sent. After that, or for any other error, the stream ends with
`{"type": "error", "error": "..."}` and no assistant message is stored.

CORS allows `localhost` and `127.0.0.1` on ports 5173 and 3000, plus
`tauri://localhost`.

## Tests

```bash
assistants-app/.venv/bin/python -m pip install -r assistants-app/requirements-dev.txt
assistants-app/.venv/bin/black --check assistants-app/
assistants-app/.venv/bin/python -m pytest assistants-app/tests/
```
