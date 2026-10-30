"""FastAPI server for the tm-agents assistants app (Slice 1.3).

Why: the desktop UI needs an HTTP API over the assistants store and a
streaming chat endpoint backed by Bedrock.
What: assistant create/list/get, message history, and
`POST /assistants/{id}/chat`, which stores the user message, streams the
reply from Bedrock `converse_stream` as newline-delimited JSON, then stores
the assistant message with its token counts and cost.
Test: `assistants-app/tests/test_server.py`.

Run from the repository root:

    uvicorn assistants-app.server:app --reload

Stream format (`application/x-ndjson`), one object per line:

    {"type": "delta", "text": "..."}
    {"type": "done", "message_id": 7, "tokens_in": 12, "tokens_out": 40,
     "cost_usd": 0.000636}
    {"type": "error", "error": "..."}            # instead of "done"
"""

from __future__ import annotations

import json
import os
import random
import sqlite3
import sys
import time
from collections.abc import AsyncIterator, Iterator
from contextlib import asynccontextmanager
from functools import lru_cache
from pathlib import Path
from typing import Any

# `uvicorn assistants-app.server:app` imports this file as a namespace-package
# member, so the sibling modules are not importable until their directory is
# on the path.
sys.path.insert(0, str(Path(__file__).resolve().parent))

import boto3  # noqa: E402
from botocore.exceptions import BotoCoreError, ClientError  # noqa: E402
from fastapi import Depends, FastAPI, HTTPException, Request, status  # noqa: E402
from fastapi.middleware.cors import CORSMiddleware  # noqa: E402
from fastapi.responses import StreamingResponse  # noqa: E402
from pydantic import BaseModel, Field, ValidationError  # noqa: E402

import db  # noqa: E402
from pricing import estimate_cost  # noqa: E402

AWS_PROFILE = os.environ.get("AWS_PROFILE", "cto")
AWS_REGION = os.environ.get("AWS_REGION", "us-east-1")

# Bedrock error codes worth retrying. Codes arrive capitalised from the
# initial call and camelCase from in-stream error events, so compare lowered.
RETRYABLE_CODES = frozenset(
    code.lower()
    for code in (
        "ThrottlingException",
        "ServiceQuotaExceededException",
        "ServiceUnavailableException",
        "ModelNotReadyException",
        "InternalServerException",
        "ModelStreamErrorException",
    )
)
MAX_ATTEMPTS = 4
RETRY_BASE_DELAY_S = 0.5

CORS_ORIGINS = [
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "http://localhost:3000",
    "http://127.0.0.1:3000",
    "tauri://localhost",
]


class AssistantCreate(BaseModel):
    """Request body for `POST /assistants`."""

    name: str = Field(min_length=1)
    instructions: str = ""
    model: str = db.DEFAULT_MODEL


class ChatRequest(BaseModel):
    """Request body for `POST /assistants/{id}/chat`."""

    content: str = Field(min_length=1)


@asynccontextmanager
async def lifespan(_: FastAPI) -> AsyncIterator[None]:
    db.init_db()
    yield


app = FastAPI(title="tm-agents assistants", lifespan=lifespan)
app.add_middleware(
    CORSMiddleware,
    allow_origins=CORS_ORIGINS,
    allow_methods=["*"],
    allow_headers=["*"],
)


@lru_cache(maxsize=1)
def get_bedrock_client() -> Any:
    """Returns the shared `bedrock-runtime` client; tests override this dependency."""
    session = boto3.Session(profile_name=AWS_PROFILE, region_name=AWS_REGION)
    return session.client("bedrock-runtime")


def _require_assistant(assistant_id: int) -> db.Assistant:
    assistant = db.get_assistant(assistant_id)
    if assistant is None:
        raise HTTPException(status.HTTP_404_NOT_FOUND, "assistant not found")
    return assistant


@app.post("/assistants", status_code=status.HTTP_201_CREATED)
def create_assistant(body: AssistantCreate) -> db.Assistant:
    try:
        new_id = db.create_assistant(body.name, body.instructions, body.model)
    except sqlite3.IntegrityError as err:
        raise HTTPException(
            status.HTTP_409_CONFLICT, f"assistant {body.name!r} already exists"
        ) from err
    return _require_assistant(new_id)


@app.get("/assistants")
def list_assistants() -> list[db.Assistant]:
    return db.list_assistants()


@app.get("/assistants/{assistant_id}")
def get_assistant(assistant_id: int) -> db.Assistant:
    return _require_assistant(assistant_id)


@app.get("/assistants/{assistant_id}/messages")
def list_messages(assistant_id: int) -> list[dict[str, Any]]:
    _require_assistant(assistant_id)
    return db.list_messages(assistant_id)


def to_bedrock_messages(history: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Converts stored messages to Converse format.

    Why: Converse requires roles to alternate starting with "user", but a
    failed reply leaves two user messages in a row.
    What: drops leading assistant messages and joins consecutive same-role
    messages with a blank line.
    Test: `test_to_bedrock_messages_merges_same_role_runs`.
    """
    merged: list[dict[str, str]] = []
    for msg in history:
        if not merged and msg["role"] != "user":
            continue
        if merged and merged[-1]["role"] == msg["role"]:
            merged[-1]["text"] += "\n\n" + msg["content"]
        else:
            merged.append({"role": msg["role"], "text": msg["content"]})
    return [{"role": m["role"], "content": [{"text": m["text"]}]} for m in merged]


def _error_code(err: ClientError) -> str:
    return str(err.response.get("Error", {}).get("Code", ""))


def _backoff_delay(attempt: int) -> float:
    """Exponential backoff with full jitter: up to base * 2**attempt seconds."""
    return random.uniform(0, RETRY_BASE_DELAY_S * (2**attempt))


def _line(obj: dict[str, Any]) -> bytes:
    return (json.dumps(obj) + "\n").encode("utf-8")


def stream_reply(
    client: Any, assistant: db.Assistant, messages: list[dict[str, Any]]
) -> Iterator[bytes]:
    """Streams one Bedrock reply as NDJSON and stores it when complete.

    Why: a throttled or quota-limited call should recover without the
    client resubmitting, but text already sent cannot be taken back.
    What: retries a retryable error with exponential backoff while no text
    has been sent; after the first delta, or for any other error, it emits an
    "error" line and stores nothing. On success it stores the assistant
    message with tokens and cost and emits a "done" line.
    Test: `test_chat_retries_throttling_then_streams`,
    `test_chat_stops_after_max_attempts`,
    `test_chat_non_retryable_error_stores_no_reply`.
    """
    request: dict[str, Any] = {"modelId": assistant["model"], "messages": messages}
    if assistant["instructions"]:
        request["system"] = [{"text": assistant["instructions"]}]

    for attempt in range(MAX_ATTEMPTS):
        parts: list[str] = []
        usage: dict[str, int] | None = None
        try:
            response = client.converse_stream(**request)
            for event in response["stream"]:
                if "contentBlockDelta" in event:
                    text = event["contentBlockDelta"]["delta"].get("text", "")
                    if text:
                        parts.append(text)
                        yield _line({"type": "delta", "text": text})
                elif "metadata" in event:
                    usage = event["metadata"].get("usage")
        except ClientError as err:
            code = _error_code(err)
            retryable = code.lower() in RETRYABLE_CODES and not parts
            if retryable and attempt + 1 < MAX_ATTEMPTS:
                print(
                    f"Bedrock {code}; retry {attempt + 1}/{MAX_ATTEMPTS - 1}",
                    file=sys.stderr,
                )
                time.sleep(_backoff_delay(attempt))
                continue
            print(f"Bedrock call failed: {err}", file=sys.stderr)
            yield _line({"type": "error", "error": f"{code}: {err}"})
            return
        except BotoCoreError as err:
            print(f"Bedrock call failed: {err}", file=sys.stderr)
            yield _line({"type": "error", "error": str(err)})
            return

        tokens_in = usage["inputTokens"] if usage else 0
        tokens_out = usage["outputTokens"] if usage else 0
        if usage is None:
            print("Bedrock stream ended without usage metadata", file=sys.stderr)
        cost = estimate_cost(tokens_in, tokens_out)
        message_id = db.add_message(
            assistant["id"], "assistant", "".join(parts), tokens_in, tokens_out, cost
        )
        yield _line(
            {
                "type": "done",
                "message_id": message_id,
                "tokens_in": tokens_in,
                "tokens_out": tokens_out,
                "cost_usd": cost,
            }
        )
        return


async def parse_chat_request(request: Request) -> ChatRequest:
    """Parses the chat body as JSON whatever its Content-Type.

    Why: `curl -d '{...}'` sends `application/x-www-form-urlencoded`, which
    FastAPI's body binding rejects.
    Test: `test_chat_accepts_json_without_content_type`.
    """
    try:
        return ChatRequest.model_validate_json(await request.body())
    except ValidationError as err:
        raise HTTPException(
            status.HTTP_422_UNPROCESSABLE_CONTENT, err.errors(include_url=False)
        ) from err


@app.post("/assistants/{assistant_id}/chat")
def chat(
    assistant_id: int,
    body: ChatRequest = Depends(parse_chat_request),
    client: Any = Depends(get_bedrock_client),
) -> StreamingResponse:
    assistant = _require_assistant(assistant_id)
    db.add_message(assistant_id, "user", body.content)
    messages = to_bedrock_messages(db.list_messages(assistant_id))
    return StreamingResponse(
        stream_reply(client, assistant, messages), media_type="application/x-ndjson"
    )
