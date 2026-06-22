"""
HTTP client for the trusty-mpm agent daemon.

Why: Isolates all HTTP concerns so the pipeline module stays focused on audio
     flow and the daemon communication contract is easy to test with respx mocks.
What: Provides DaemonClient — a thin async wrapper around httpx that calls
     POST /api/v1/sessions/chat, handles errors, and returns the reply text.
Test: Use respx to mock httpx; call send_message and assert the correct text
     is extracted.  Test error paths (non-200, bad JSON) with mocked responses.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

import httpx

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Types
# ---------------------------------------------------------------------------


@dataclass
class ChatResponse:
    """Parsed response from the agent daemon.

    Why: A typed object is safer to pass around than raw dicts.
    What: Holds the reply text and the conversation id for multi-turn sessions.
    Test: Construct directly and assert field values.
    """

    text: str
    conv_id: str | None = None
    raw: dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Client
# ---------------------------------------------------------------------------


class DaemonClient:
    """Async HTTP client for the trusty-mpm local agent endpoint.

    Why: Keeps HTTP plumbing out of the pipeline module so it can be replaced
         or mocked without touching audio code.
    What: Wraps httpx.AsyncClient; sends one POST per user utterance; returns
         ChatResponse with the assistant reply and the updated conv_id.
    Test: Instantiate with base_url='http://test'; use respx.mock to intercept
         the POST and return a fixture payload.
    """

    CHAT_PATH = "/api/v1/sessions/chat"

    def __init__(self, base_url: str, timeout: float = 60.0) -> None:
        """
        Why: Timeout is exposed so callers can increase it for slow LLM
             responses without patching the class.
        What: Creates a reusable httpx.AsyncClient with the given base URL.
        Test: Assert self._client.base_url == base_url.
        """
        self._client = httpx.AsyncClient(base_url=base_url, timeout=timeout)

    async def send_message(self, text: str, conv_id: str | None = None) -> ChatResponse:
        """Send a user utterance and return the agent reply.

        Why: Single-responsibility method keeps error handling in one place.
        What: POSTs {"message": text, "conv_id": conv_id} to /api/v1/sessions/chat.
              Extracts "response" (or "text") key from the JSON body and
              returns it as ChatResponse.
        Test: Mock POST with respx; assert method=POST, JSON body keys, and
              that the returned ChatResponse.text matches the fixture reply.
        """
        payload: dict[str, Any] = {"message": text}
        if conv_id is not None:
            payload["conv_id"] = conv_id

        logger.debug("daemon → POST %s payload=%r", self.CHAT_PATH, {**payload})

        try:
            resp = await self._client.post(self.CHAT_PATH, json=payload)
            resp.raise_for_status()
        except httpx.HTTPStatusError as exc:
            raise DaemonError(
                f"Daemon returned HTTP {exc.response.status_code}: {exc.response.text[:200]}"
            ) from exc
        except httpx.RequestError as exc:
            raise DaemonError(f"Failed to reach daemon at {self._client.base_url}: {exc}") from exc

        try:
            data: dict[str, Any] = resp.json()
        except Exception as exc:
            raise DaemonError(f"Daemon returned non-JSON body: {resp.text[:200]}") from exc

        # Support multiple possible reply key names
        reply_text: str = (
            data.get("response")
            or data.get("text")
            or data.get("reply")
            or data.get("content")
            or ""
        )
        if not reply_text:
            logger.warning("Daemon response had no recognisable text key. keys=%s", list(data))

        new_conv_id: str | None = data.get("conv_id") or conv_id

        logger.debug("daemon ← reply len=%d conv_id=%s", len(reply_text), new_conv_id)
        return ChatResponse(text=reply_text, conv_id=new_conv_id, raw=data)

    async def aclose(self) -> None:
        """Close the underlying HTTP client.

        Why: Prevents resource-leak warnings when the client is not used as a
             context manager.
        What: Delegates to httpx.AsyncClient.aclose().
        Test: Call aclose() then assert no further requests succeed.
        """
        await self._client.aclose()

    async def __aenter__(self) -> DaemonClient:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------


class DaemonError(RuntimeError):
    """Raised when the agent daemon returns an unexpected response.

    Why: Typed exception allows callers to handle daemon failures specifically
         (e.g., speak an error phrase instead of crashing).
    What: Subclass of RuntimeError; carries a human-readable message.
    Test: Raise and catch in tests; assert isinstance(exc, DaemonError).
    """
