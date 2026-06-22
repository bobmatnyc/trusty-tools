"""
Text-to-speech helpers for trusty-voice Phase 0.

Why: Decoupling TTS from the pipeline allows provider swaps and makes the
     audio-output step unit-testable without a speaker or API key.
What: Provides ElevenLabsSpeaker — a thin async wrapper around the ElevenLabs
     ≥1.x Python SDK (AsyncElevenLabs client) that synthesises text and writes
     PCM audio to a pyaudio stream.  Also provides TextOnlySpeaker used in
     dry-run / text mode.
Test: Inject a mock ElevenLabs client whose text_to_speech.stream() returns an
     async iterable of bytes; mock pyaudio stream.write; call speak(); assert
     that write() received the expected chunks.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# ElevenLabs ≥1.x implementation
# ---------------------------------------------------------------------------


class ElevenLabsSpeaker:
    """Synthesises text via ElevenLabs ≥1.x and plays through the default audio device.

    Why: The ElevenLabs Python SDK ≥1.0 removed the module-level generate() /
         set_api_key() helpers and replaced them with an explicit client class.
         Using AsyncElevenLabs lets speak() stay a first-class async method so
         it integrates cleanly with the asyncio pipeline loop.
    What: Uses AsyncElevenLabs.text_to_speech.stream() with output_format="pcm_22050"
         to receive raw 16-bit mono PCM chunks (no decoder needed) and writes them
         directly to a pyaudio output stream at 22 050 Hz.
    Test: Inject a mock async_elevenlabs_client whose text_to_speech.stream returns
         an async iterator of bytes; mock pyaudio.PyAudio and stream.write; assert
         chunks forwarded in order and stream is always closed.
    """

    # pcm_22050 → 16-bit / mono / 22 050 Hz raw PCM
    SAMPLE_RATE = 22050
    CHANNELS = 1
    SAMPLE_WIDTH = 2  # 16-bit

    def __init__(
        self,
        api_key: str,
        # "Rachel" (21m00Tcm4TlvDq8ikWAM) was retired by ElevenLabs — do not
        # revert to that id.  "River" (SAz9YHcvj6GT2YYXdXww) is a current
        # premade conversational voice.
        voice_id: str = "SAz9YHcvj6GT2YYXdXww",  # ElevenLabs "River"
        model_id: str = "eleven_turbo_v2",
        *,
        _client: Any = None,  # injection point for tests
    ) -> None:
        """
        Why: Stores credentials and accepts an optional mock client for testing.
        What: Creates an AsyncElevenLabs client lazily on first speak(); stores
             config; caches the pyaudio PyAudio instance.
        Test: Pass _client=mock_async_client; construct and assert attributes;
             no audio device or network required.
        """
        self._api_key = api_key
        self._voice_id = voice_id
        self._model_id = model_id
        self._el_client: Any = _client  # AsyncElevenLabs, lazily created
        self._pa: Any = None  # pyaudio.PyAudio, lazily initialised

    def _get_el_client(self) -> Any:
        """Lazily create the AsyncElevenLabs client.

        Why: Defers SDK import so tests that inject _client never touch real ElevenLabs.
        What: Imports elevenlabs.client.AsyncElevenLabs, constructs with api_key, caches.
        Test: Monkeypatch elevenlabs.client before calling; assert _el_client is set.
        """
        if self._el_client is None:
            from elevenlabs.client import AsyncElevenLabs  # type: ignore[import]

            self._el_client = AsyncElevenLabs(api_key=self._api_key)
        return self._el_client

    def _get_pa(self) -> Any:
        """Lazily initialise pyaudio to avoid device probing at import time.

        Why: pyaudio.PyAudio() touches the audio subsystem on construction, which
             can fail or be slow in CI / headless environments.
        What: Creates PyAudio() on first call and caches it.
        Test: Patch pyaudio.PyAudio in tests to avoid device access.
        """
        if self._pa is None:
            import pyaudio  # type: ignore[import]

            self._pa = pyaudio.PyAudio()
        return self._pa

    async def speak(self, text: str) -> None:
        """Synthesise text and play audio to the default output device.

        Why: async-friendly so it integrates with the asyncio pipeline loop without
             blocking the event loop for long audio segments.
        What: Calls AsyncElevenLabs.text_to_speech.stream() which returns an
             AsyncIterator[bytes] of raw PCM at 22 050 Hz / 16-bit mono; opens a
             pyaudio output stream; forwards each chunk via stream.write(); closes
             the pyaudio stream in a finally block.
        Test: Mock _get_el_client() to return an object whose text_to_speech.stream
             returns an async generator yielding known byte chunks; mock pyaudio;
             assert write() called with each chunk in order.
        """
        if not text.strip():
            logger.debug("tts: empty text — skipping synthesis")
            return

        logger.debug("tts → synthesising %d chars", len(text))

        import pyaudio  # type: ignore[import]

        client = self._get_el_client()
        pa = self._get_pa()

        stream = pa.open(
            format=pyaudio.paInt16,
            channels=self.CHANNELS,
            rate=self.SAMPLE_RATE,
            output=True,
        )
        try:
            async for chunk in client.text_to_speech.stream(
                voice_id=self._voice_id,
                text=text,
                model_id=self._model_id,
                output_format="pcm_22050",
            ):
                if chunk:
                    stream.write(chunk)
        finally:
            stream.stop_stream()
            stream.close()

        logger.debug("tts ← playback complete")

    def close(self) -> None:
        """Release pyaudio resources.

        Why: Prevents resource-leak warnings at process exit.
        What: Terminates the PyAudio instance if it was ever opened.
        Test: Call close(); verify no exception even if pyaudio never initialised.
        """
        if self._pa is not None:
            self._pa.terminate()
            self._pa = None


# ---------------------------------------------------------------------------
# Dry-run / text mode implementation
# ---------------------------------------------------------------------------


class TextOnlySpeaker:
    """Fake speaker for dry-run / text mode — prints instead of playing audio.

    Why: Allows full pipeline testing without audio hardware or a live API key.
    What: speak() simply prints the text to stdout prefixed with [TTS].
    Test: Capture stdout and assert the expected prefix + text appear.
    """

    async def speak(self, text: str) -> None:
        """Print the text that would be spoken.

        Why: Gives developers a visible confirmation the TTS step was reached.
        What: Prints to stdout.
        Test: Redirect stdout; call speak("hello"); assert "[TTS]" in output.
        """
        print(f"[TTS] {text}")

    def close(self) -> None:
        """No-op; present for interface symmetry with ElevenLabsSpeaker."""
