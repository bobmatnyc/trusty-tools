"""Tests for `server.py` endpoints, with a scripted fake Bedrock client."""

from __future__ import annotations

import json
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest
from botocore.exceptions import ClientError
from fastapi.testclient import TestClient

import db
import pricing
import server


def client_error(code: str) -> ClientError:
    return ClientError({"Error": {"Code": code, "Message": code}}, "ConverseStream")


def reply_events(*texts: str, tokens_in: int = 10, tokens_out: int = 20) -> list[Any]:
    events: list[Any] = [{"contentBlockDelta": {"delta": {"text": t}}} for t in texts]
    events.append(
        {"metadata": {"usage": {"inputTokens": tokens_in, "outputTokens": tokens_out}}}
    )
    return events


class FakeBedrock:
    """Plays one scripted outcome per `converse_stream` call.

    An outcome is an exception (raised by the call) or a list of events, where
    an exception in the list is raised mid-stream.
    """

    def __init__(self, *outcomes: Any) -> None:
        self.outcomes = list(outcomes)
        self.requests: list[dict[str, Any]] = []

    def converse_stream(self, **request: Any) -> dict[str, Any]:
        self.requests.append(request)
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, Exception):
            raise outcome
        return {"stream": self._iterate(outcome)}

    @staticmethod
    def _iterate(events: list[Any]) -> Iterator[dict[str, Any]]:
        for event in events:
            if isinstance(event, Exception):
                raise event
            yield event


@pytest.fixture
def api(db_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[TestClient]:
    monkeypatch.setenv("ASSISTANTS_DB_PATH", str(db_path))
    monkeypatch.setattr(server, "RETRY_BASE_DELAY_S", 0.0)
    with TestClient(server.app) as client:
        yield client
    server.app.dependency_overrides.clear()


def use_bedrock(fake: FakeBedrock) -> FakeBedrock:
    server.app.dependency_overrides[server.get_bedrock_client] = lambda: fake
    return fake


def make_assistant(api: TestClient, name: str = "General") -> dict[str, Any]:
    resp = api.post("/assistants", json={"name": name, "instructions": "Be brief."})
    assert resp.status_code == 201, resp.text
    return resp.json()


def chat_lines(api: TestClient, assistant_id: int, content: str) -> list[dict]:
    resp = api.post(f"/assistants/{assistant_id}/chat", json={"content": content})
    assert resp.status_code == 200, resp.text
    assert resp.headers["content-type"].startswith("application/x-ndjson")
    return [json.loads(line) for line in resp.text.splitlines()]


def test_estimate_cost_uses_sonnet_pricing() -> None:
    assert pricing.estimate_cost(1_000_000, 1_000_000) == pytest.approx(18.0)


def test_assistant_create_list_get_flow(api: TestClient) -> None:
    created = make_assistant(api)

    assert created["name"] == "General"
    assert created["model"] == db.DEFAULT_MODEL
    assert api.get("/assistants").json() == [created]
    assert api.get(f"/assistants/{created['id']}").json() == created


def test_create_assistant_rejects_duplicate_name(api: TestClient) -> None:
    make_assistant(api)

    resp = api.post("/assistants", json={"name": "General"})

    assert resp.status_code == 409


@pytest.mark.parametrize(
    "method,path",
    [
        ("get", "/assistants/9999"),
        ("get", "/assistants/9999/messages"),
        ("post", "/assistants/9999/chat"),
    ],
)
def test_unknown_assistant_is_404(api: TestClient, method: str, path: str) -> None:
    kwargs = {"json": {"content": "Hi"}} if method == "post" else {}
    assert getattr(api, method)(path, **kwargs).status_code == 404


def test_chat_streams_deltas_and_stores_both_messages(api: TestClient) -> None:
    fake = use_bedrock(FakeBedrock(reply_events("Hel", "lo!")))
    assistant = make_assistant(api)

    lines = chat_lines(api, assistant["id"], "Hello")

    assert lines[:2] == [
        {"type": "delta", "text": "Hel"},
        {"type": "delta", "text": "lo!"},
    ]
    done = lines[2]
    assert done["type"] == "done"
    assert (done["tokens_in"], done["tokens_out"]) == (10, 20)
    assert done["cost_usd"] == pytest.approx(pricing.estimate_cost(10, 20))
    assert fake.requests[0]["modelId"] == db.DEFAULT_MODEL
    assert fake.requests[0]["system"] == [{"text": "Be brief."}]

    history = api.get(f"/assistants/{assistant['id']}/messages").json()
    assert [(m["role"], m["content"]) for m in history] == [
        ("user", "Hello"),
        ("assistant", "Hello!"),
    ]
    reply = history[1]
    assert reply["id"] == done["message_id"]
    assert (reply["tokens_in"], reply["tokens_out"]) == (10, 20)
    assert reply["cost_usd"] == pytest.approx(done["cost_usd"])


def test_chat_sends_prior_history(api: TestClient) -> None:
    fake = use_bedrock(FakeBedrock(reply_events("One"), reply_events("Two")))
    assistant = make_assistant(api)

    chat_lines(api, assistant["id"], "First")
    chat_lines(api, assistant["id"], "Second")

    assert [m["role"] for m in fake.requests[1]["messages"]] == [
        "user",
        "assistant",
        "user",
    ]


def test_chat_retries_throttling_then_streams(api: TestClient) -> None:
    fake = use_bedrock(
        FakeBedrock(
            client_error("ThrottlingException"),
            [client_error("throttlingException")],
            reply_events("Ok"),
        )
    )
    assistant = make_assistant(api)

    lines = chat_lines(api, assistant["id"], "Hello")

    assert len(fake.requests) == 3
    assert [line["type"] for line in lines] == ["delta", "done"]


def test_chat_stops_after_max_attempts(api: TestClient) -> None:
    errors = [client_error("ServiceQuotaExceededException")] * server.MAX_ATTEMPTS
    fake = use_bedrock(FakeBedrock(*errors))
    assistant = make_assistant(api)

    lines = chat_lines(api, assistant["id"], "Hello")

    assert len(fake.requests) == server.MAX_ATTEMPTS
    assert [line["type"] for line in lines] == ["error"]
    assert "ServiceQuotaExceededException" in lines[0]["error"]


@pytest.mark.parametrize(
    "outcome",
    [
        client_error("ValidationException"),
        reply_events("Partial")[:1] + [client_error("throttlingException")],
    ],
    ids=["non-retryable", "throttled-after-first-delta"],
)
def test_chat_non_retryable_error_stores_no_reply(
    api: TestClient, outcome: Any
) -> None:
    fake = use_bedrock(FakeBedrock(outcome))
    assistant = make_assistant(api)

    lines = chat_lines(api, assistant["id"], "Hello")

    assert len(fake.requests) == 1
    assert lines[-1]["type"] == "error"
    history = api.get(f"/assistants/{assistant['id']}/messages").json()
    assert [m["role"] for m in history] == ["user"]


def test_to_bedrock_messages_merges_same_role_runs() -> None:
    history = [
        {"role": "assistant", "content": "orphan"},
        {"role": "user", "content": "a"},
        {"role": "user", "content": "b"},
        {"role": "assistant", "content": "c"},
    ]

    assert server.to_bedrock_messages(history) == [
        {"role": "user", "content": [{"text": "a\n\nb"}]},
        {"role": "assistant", "content": [{"text": "c"}]},
    ]


def test_chat_accepts_json_without_content_type(api: TestClient) -> None:
    use_bedrock(FakeBedrock(reply_events("Hi")))
    assistant = make_assistant(api)
    url = f"/assistants/{assistant['id']}/chat"
    form = {"content-type": "application/x-www-form-urlencoded"}

    ok = api.post(url, content='{"content": "Hello"}', headers=form)
    empty = api.post(url, content='{"content": ""}', headers=form)

    assert ok.status_code == 200
    types = [json.loads(line)["type"] for line in ok.text.splitlines()]
    assert types == ["delta", "done"]
    assert empty.status_code == 422
