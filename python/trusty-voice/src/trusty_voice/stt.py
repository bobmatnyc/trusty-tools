"""
Speech-to-text helpers for trusty-voice Phase 0.

Why: Decoupling STT from the pipeline lets us swap providers (Deepgram → Whisper)
     without changing pipeline wiring and makes unit-testing trivial via mocks.
What: Provides DeepgramTranscriber — a thin async wrapper around the Deepgram
     Python SDK's pre-recorded (batch) transcription used in Phase 0.  Phase 1
     will add streaming real-time transcription.
Test: Inject a mock deepgram client; call transcribe_bytes; assert the extracted
     transcript text matches the fixture response.
"""

from __future__ import annotations

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
    """Async wrapper around the Deepgram pre-recorded transcription API.

    Why: Phase 0 uses push-to-talk (record a buffer, then transcribe) rather
         than streaming, so the pre-recorded API is the simplest fit.
    What: Holds the Deepgram client; exposes transcribe_bytes() which sends
         raw audio bytes (WAV/PCM) and returns TranscriptResult.
    Test: Pass a mock client whose listen.asyncprerecorded.v("1").transcribe_file()
         returns a fixture; assert the returned TranscriptResult.text.
    """

    def __init__(
        self,
        api_key: str,
        language: str = "en-US",
        model: str = "nova-2",
    ) -> None:
        """
        Why: Defer import so tests can mock deepgram_sdk without installing it.
        What: Creates a Deepgram AsyncClient; stores options.
        Test: Monkeypatch deepgram_sdk.AsyncDeepgramClient before instantiating.
        """
        from deepgram import AsyncDeepgramClient, PrerecordedOptions  # type: ignore[import]

        self._client = AsyncDeepgramClient(api_key)
        self._options = PrerecordedOptions(
            model=model,
            language=language,
            smart_format=True,
            punctuate=True,
        )

    async def transcribe_bytes(
        self, audio_bytes: bytes, mimetype: str = "audio/wav"
    ) -> TranscriptResult:
        """Transcribe a raw audio buffer via Deepgram pre-recorded API.

        Why: Cleanly separates the "send bytes → get text" contract from the
             SDK's nested response structure.
        What: Posts audio_bytes to Deepgram; extracts the first transcript
             alternative from the response.
        Test: Patch self._client; assert mimetype is forwarded and text is
              extracted from the fixture response shape.
        """
        from deepgram import FileSource  # type: ignore[import]

        payload: FileSource = {"buffer": audio_bytes, "mimetype": mimetype}

        logger.debug("stt → sending %d bytes to Deepgram", len(audio_bytes))
        response = await self._client.listen.asyncprerecorded.v("1").transcribe_file(
            payload, self._options
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
        import contextlib

        with contextlib.suppress(Exception):
            raw = response.to_dict()  # type: ignore[attr-defined]

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
