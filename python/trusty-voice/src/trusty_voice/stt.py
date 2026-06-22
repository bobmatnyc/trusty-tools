"""
Speech-to-text helpers for trusty-voice Phase 0.

Why: Decoupling STT from the pipeline lets us swap providers (Deepgram → Whisper)
     without changing pipeline wiring and makes unit-testing trivial via mocks.
What: Provides DeepgramTranscriber — a thin async wrapper around the Deepgram
     Python SDK v7+ pre-recorded (batch) transcription used in Phase 0.  Phase 1
     will add streaming real-time transcription.
Test: Inject a mock deepgram client via _client; call transcribe_bytes; assert the
     extracted transcript text matches the fixture response.
"""

from __future__ import annotations

import contextlib
import logging
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Types
# ---------------------------------------------------------------------------


@dataclass
class TranscriptResult:
    """Result of a single STT transcription call.

    Why: Typed wrapper prevents callers from drilling into nested SDK response dicts.
    What: Carries the transcript text and the raw SDK response for debugging.
    Test: Construct directly; assert .text.
    """

    text: str
    confidence: float = 0.0
    raw: dict[str, Any] | None = None


# ---------------------------------------------------------------------------
# Transcriber
# ---------------------------------------------------------------------------


class DeepgramTranscriber:
    """Async wrapper around the Deepgram ≥7.x pre-recorded transcription API.

    Why: Phase 0 uses push-to-talk (record a buffer, then transcribe) rather
         than streaming, so the pre-recorded API is the simplest fit.
    What: Holds the Deepgram async client; exposes transcribe_bytes() which posts
         raw audio bytes (WAV/PCM) to client.listen.v1.media.transcribe_file()
         and returns a TranscriptResult.
    Test: Pass _client=mock_client whose listen.v1.media.transcribe_file() is an
         AsyncMock returning a fixture; assert the returned TranscriptResult.text.

    SDK compatibility note (deepgram-sdk ≥7):
      - Constructor: AsyncDeepgramClient(api_key=<key>)   (keyword-only)
      - Transcribe:  await client.listen.v1.media.transcribe_file(
                         request=<bytes>,
                         model=..., language=..., smart_format=..., punctuate=...
                     )
      - No PrerecordedOptions class; options are flat keyword args.
      - Response:    ListenV1Response → .results.channels[0].alternatives[0].transcript
    """

    def __init__(
        self,
        api_key: str,
        language: str = "en-US",
        model: str = "nova-2",
        *,
        _client: Any = None,  # injection point for tests (no network)
    ) -> None:
        """
        Why: Accept an optional mock client so tests never touch the real SDK or network.
        What: Stores credentials and transcription options; creates a real
             AsyncDeepgramClient lazily via _get_client() unless _client is injected.
        Test: Pass _client=mock; construct; assert no import of real deepgram occurs.
        """
        self._api_key = api_key
        self._language = language
        self._model = model
        self._dg_client: Any = _client  # AsyncDeepgramClient, lazily created

    def _get_client(self) -> Any:
        """Lazily create the AsyncDeepgramClient.

        Why: Defers SDK import so tests that inject _client never touch real Deepgram.
        What: Imports deepgram.AsyncDeepgramClient, constructs with api_key=, caches.
        Test: Monkeypatch deepgram before calling; assert _dg_client is set.
        """
        if self._dg_client is None:
            from deepgram import AsyncDeepgramClient  # type: ignore[import]

            self._dg_client = AsyncDeepgramClient(api_key=self._api_key)
        return self._dg_client

    async def transcribe_bytes(
        self, audio_bytes: bytes, mimetype: str = "audio/wav"
    ) -> TranscriptResult:
        """Transcribe a raw audio buffer via Deepgram pre-recorded API (v7 SDK).

        Why: Cleanly separates the "send bytes → get text" contract from the
             SDK's nested response structure.
        What: Posts audio_bytes to client.listen.v1.media.transcribe_file();
             extracts the first transcript alternative from the response.
        Test: Inject a mock _client; assert transcribe_file is called with
              request=audio_bytes and the expected option kwargs; assert the
              returned TranscriptResult.text matches the fixture value.
        """
        client = self._get_client()

        logger.debug("stt → sending %d bytes to Deepgram", len(audio_bytes))
        response = await client.listen.v1.media.transcribe_file(
            request=audio_bytes,
            model=self._model,
            language=self._language,
            smart_format=True,
            punctuate=True,
        )

        try:
            alt = response.results.channels[0].alternatives[0]
            text: str = alt.transcript or ""
            confidence: float = float(getattr(alt, "confidence", 0.0))
        except (IndexError, AttributeError) as exc:
            logger.warning("Could not extract transcript from Deepgram response: %s", exc)
            text = ""
            confidence = 0.0

        raw: dict[str, Any] = {}
        with contextlib.suppress(Exception):
            raw = response.model_dump()  # ListenV1Response is a Pydantic v2 model

        logger.debug("stt ← transcript=%r confidence=%.2f", text, confidence)
        return TranscriptResult(text=text, confidence=confidence, raw=raw)


# ---------------------------------------------------------------------------
# Mock / dry-run implementation
# ---------------------------------------------------------------------------


class TextPassthroughTranscriber:
    """Fake transcriber used in text (dry-run) mode.

    Why: Allows the full pipeline to be exercised without audio hardware or
         a live Deepgram key by accepting typed input as the "transcript".
    What: transcribe_bytes() is a no-op; the real input comes from stdin via
         the text-mode loop in __main__.
    Test: Instantiate; assert transcribe_bytes returns empty TranscriptResult.
    """

    async def transcribe_bytes(
        self, audio_bytes: bytes, mimetype: str = "audio/wav"
    ) -> TranscriptResult:
        """Return empty transcript — text mode bypasses audio capture."""
        return TranscriptResult(text="", confidence=1.0)
