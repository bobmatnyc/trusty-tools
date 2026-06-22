"""
Text-to-speech helpers for trusty-voice Phase 0.

Why: Decoupling TTS from the pipeline allows provider swaps and makes the
     audio-output step unit-testable without a speaker or API key.
What: Provides ElevenLabsSpeaker — a thin async wrapper around the ElevenLabs
     Python SDK that synthesises text and writes PCM audio to a pyaudio stream.
     Also provides a TextOnlySpeaker used in dry-run / text mode.
Test: Inject mock ElevenLabs client and mock pyaudio stream; call speak(); assert
     that generate() was called with the expected text and that stream.write()
     received the audio chunks.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# ElevenLabs implementation
# ---------------------------------------------------------------------------


class ElevenLabsSpeaker:
    """Synthesises text via ElevenLabs and plays through the default audio device.

    Why: Keeps TTS + audio-output as one unit because the ElevenLabs streaming
         iterator naturally feeds into a pyaudio write loop.
    What: Uses elevenlabs.generate() with streaming=True to pull audio chunks and
         writes them directly to a pyaudio output stream at 22050 Hz / 16-bit mono.
    Test: Mock elevenlabs.generate to return a list of bytes; mock pyaudio.PyAudio
         and stream.write; assert chunks forwarded in order.
    """

    # ElevenLabs turbo models output at 22050 Hz by default.
    SAMPLE_RATE = 22050
    CHANNELS = 1
    SAMPLE_WIDTH = 2  # 16-bit

    def __init__(
        self,
        api_key: str,
        voice_id: str = "21m00Tcm4TlvDq8ikWAM",
        model_id: str = "eleven_turbo_v2",
    ) -> None:
        """
        Why: Defer heavy SDK imports so tests can mock without installing.
        What: Stores credentials; pyaudio is initialised lazily on first speak().
        Test: Construct and assert attributes; no audio device required.
        """
        self._api_key = api_key
        self._voice_id = voice_id
        self._model_id = model_id
        self._pa: Any = None  # pyaudio.PyAudio, lazily initialised

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

        Why: async-friendly so it integrates with the asyncio pipeline loop
             without blocking the event loop for long audio segments.
        What: Calls elevenlabs.generate() synchronously in a thread executor
             (the SDK doesn't have a native async API for streaming), then
             writes chunks to pyaudio in the event loop.
        Test: Mock generate; mock stream.write; assert called with text.
        """
        if not text.strip():
            logger.debug("tts: empty text — skipping synthesis")
            return

        logger.debug("tts → synthesising %d chars", len(text))

        import pyaudio  # type: ignore[import]
        from elevenlabs import generate, set_api_key  # type: ignore[import]

        set_api_key(self._api_key)

        loop = asyncio.get_running_loop()

        def _generate_and_play() -> None:
            audio_iter = generate(
                text=text,
                voice=self._voice_id,
                model=self._model_id,
                stream=True,
            )
            pa = self._get_pa()
            stream = pa.open(
                format=pyaudio.paInt16,
                channels=self.CHANNELS,
                rate=self.SAMPLE_RATE,
                output=True,
            )
            try:
                for chunk in audio_iter:
                    if chunk:
                        stream.write(chunk)
            finally:
                stream.stop_stream()
                stream.close()

        await loop.run_in_executor(None, _generate_and_play)
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
