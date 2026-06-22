"""
Tests for trusty_voice.daemon_client.

Coverage:
- Happy-path POST with reply extracted from various key names
- HTTP error (non-200) raises DaemonError
- Network error raises DaemonError
- Non-JSON body raises DaemonError
- conv_id forwarded in payload when provided
- conv_id updated from response
"""

from __future__ import annotations

import httpx
import pytest
import respx

from trusty_voice.daemon_client import ChatResponse, DaemonClient, DaemonError

DAEMON_URL = "http://127.0.0.1:7880"
CHAT_PATH = "/api/v1/sessions/chat"


# ---------------------------------------------------------------------------
# Happy path
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
@respx.mock
async def test_send_message_happy_path() -> None:
    """send_message extracts 'response' key from daemon JSON reply."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(
            200,
            json={"response": "Hello from agent", "conv_id": "sess-1"},
        )
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        result = await client.send_message("hi")

    assert result.text == "Hello from agent"
    assert result.conv_id == "sess-1"


@pytest.mark.asyncio
@respx.mock
async def test_send_message_text_key_fallback() -> None:
    """send_message falls back to 'text' key when 'response' is absent."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"text": "Alt reply"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        result = await client.send_message("hi")

    assert result.text == "Alt reply"


@pytest.mark.asyncio
@respx.mock
async def test_send_message_reply_key_fallback() -> None:
    """send_message falls back to 'reply' key."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"reply": "Reply key text"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        result = await client.send_message("hi")

    assert result.text == "Reply key text"


@pytest.mark.asyncio
@respx.mock
async def test_send_message_content_key_fallback() -> None:
    """send_message falls back to 'content' key."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"content": "Content text"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        result = await client.send_message("hi")

    assert result.text == "Content text"


@pytest.mark.asyncio
@respx.mock
async def test_send_message_forwards_conv_id() -> None:
    """conv_id is included in the request payload when provided."""
    route = respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"response": "ok", "conv_id": "sess-2"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        await client.send_message("hello", conv_id="sess-1")

    sent_body = route.calls.last.request.content
    import json

    payload = json.loads(sent_body)
    assert payload["conv_id"] == "sess-1"
    assert payload["message"] == "hello"


@pytest.mark.asyncio
@respx.mock
async def test_send_message_no_conv_id_omits_key() -> None:
    """When conv_id is None, it is not sent in the request body."""
    route = respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"response": "ok"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        await client.send_message("hello", conv_id=None)

    import json

    payload = json.loads(route.calls.last.request.content)
    assert "conv_id" not in payload


@pytest.mark.asyncio
@respx.mock
async def test_send_message_conv_id_updated_from_response() -> None:
    """conv_id returned by daemon is reflected in ChatResponse."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(200, json={"response": "hi", "conv_id": "new-session-id"})
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        result = await client.send_message("msg")

    assert result.conv_id == "new-session-id"


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
@respx.mock
async def test_send_message_http_error_raises_daemon_error() -> None:
    """Non-200 HTTP response raises DaemonError."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(500, text="Internal Server Error")
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        with pytest.raises(DaemonError, match="500"):
            await client.send_message("hi")


@pytest.mark.asyncio
@respx.mock
async def test_send_message_404_raises_daemon_error() -> None:
    """404 raises DaemonError with status code in message."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(return_value=httpx.Response(404, text="Not Found"))
    async with DaemonClient(base_url=DAEMON_URL) as client:
        with pytest.raises(DaemonError, match="404"):
            await client.send_message("hi")


@pytest.mark.asyncio
@respx.mock
async def test_send_message_network_error_raises_daemon_error() -> None:
    """Connection error raises DaemonError."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(side_effect=httpx.ConnectError("refused"))
    async with DaemonClient(base_url=DAEMON_URL) as client:
        with pytest.raises(DaemonError, match="Failed to reach daemon"):
            await client.send_message("hi")


@pytest.mark.asyncio
@respx.mock
async def test_send_message_bad_json_raises_daemon_error() -> None:
    """Non-JSON body raises DaemonError."""
    respx.post(DAEMON_URL + CHAT_PATH).mock(
        return_value=httpx.Response(
            200, content=b"not json", headers={"content-type": "text/plain"}
        )
    )
    async with DaemonClient(base_url=DAEMON_URL) as client:
        with pytest.raises(DaemonError, match="non-JSON"):
            await client.send_message("hi")


# ---------------------------------------------------------------------------
# ChatResponse type
# ---------------------------------------------------------------------------


def test_chat_response_defaults() -> None:
    """ChatResponse stores text and defaults conv_id to None."""
    r = ChatResponse(text="hello")
    assert r.text == "hello"
    assert r.conv_id is None
    assert r.raw == {}


def test_chat_response_with_conv_id() -> None:
    """ChatResponse stores conv_id when provided."""
    r = ChatResponse(text="hi", conv_id="abc")
    assert r.conv_id == "abc"
