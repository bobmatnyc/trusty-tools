"""
Microphone capture for push-to-talk (Phase 0).

Why: Isolates all pyaudio record logic so the pipeline can unit-test the STT
     and daemon steps without a microphone present.
What: Provides MicRecorder — records while a key is held (or for a fixed
     duration in non-interactive mode) and returns raw PCM bytes suitable for
     the Deepgram pre-recorded API.
Test: Patch pyaudio.PyAudio in tests; call record_blocking() with duration=0.1;
      assert bytes returned are the expected mock chunks.
"""

from __future__ import annotations

import logging
import wave
from io import BytesIO
from typing import Any

logger = logging.getLogger(__name__)


class MicRecorder:
    """Records audio from the default mic until release (push-to-talk).

    Why: Push-to-talk is the simplest Phase 0 UX — hold Space, speak, release.
         This class owns all record/stop state so the pipeline stays clean.
    What: Opens a pyaudio input stream at 16 kHz / 16-bit mono (best for
         Deepgram nova-2); captures frames into a buffer; returns WAV bytes.
    Test: Monkeypatch pyaudio.PyAudio so no hardware is needed; call
         record_for_seconds(); assert returned bytes parse as a valid WAV file.
    """

    SAMPLE_RATE = 16000
    CHANNELS = 1
    SAMPLE_WIDTH = 2  # 16-bit
    CHUNK = 1024  # frames per read

    def __init__(self) -> None:
        """
        Why: Defer pyaudio import so tests can monkeypatch before instantiation.
        What: Stores None placeholders; real PyAudio initialised lazily.
        Test: Instantiate without pyaudio installed (mock the import).
        """
        self._pa: Any = None
        self._stream: Any = None

    def _get_pa(self) -> Any:
        """Lazily initialise pyaudio.

        Why: Avoids device probing at import/class-creation time (breaks CI).
        What: Creates PyAudio(); caches in self._pa.
        Test: Monkeypatch pyaudio.PyAudio before calling; assert _pa is set.
        """
        if self._pa is None:
            import pyaudio  # type: ignore[import]

            self._pa = pyaudio.PyAudio()
        return self._pa

    def record_for_seconds(self, duration: float) -> bytes:
        """Record `duration` seconds of mic audio and return WAV bytes.

        Why: Provides a simple timed record mode useful for scripted testing
             without keyboard interaction.
        What: Opens the default input stream, reads ceil(duration * rate / CHUNK)
             chunks, closes the stream, encodes as WAV, returns bytes.
        Test: Mock pyaudio; assert read() called expected number of times.
        """
        import math

        import pyaudio  # type: ignore[import]

        pa = self._get_pa()
        num_chunks = math.ceil(duration * self.SAMPLE_RATE / self.CHUNK)

        stream = pa.open(
            format=pyaudio.paInt16,
            channels=self.CHANNELS,
            rate=self.SAMPLE_RATE,
            input=True,
            frames_per_buffer=self.CHUNK,
        )
        frames: list[bytes] = []
        try:
            for _ in range(num_chunks):
                frames.append(stream.read(self.CHUNK, exception_on_overflow=False))
        finally:
            stream.stop_stream()
            stream.close()

        return self._frames_to_wav(frames)

    def record_blocking(self) -> bytes:
        """Record until Enter is pressed; return WAV bytes.

        Why: Interactive push-to-talk UX for terminal use without a dedicated
             keyboard hook library.
        What: Starts streaming in background thread; blocks on input(); stops
             stream; encodes WAV; returns bytes.
        Test: Use threading + monkey-patched input() to simulate key release.
        """
        import threading

        import pyaudio  # type: ignore[import]

        pa = self._get_pa()
        frames: list[bytes] = []
        recording = threading.Event()
        recording.set()

        stream = pa.open(
            format=pyaudio.paInt16,
            channels=self.CHANNELS,
            rate=self.SAMPLE_RATE,
            input=True,
            frames_per_buffer=self.CHUNK,
        )

        def _capture() -> None:
            # The capture thread owns the stream lifecycle: it reads until the
            # recording event is cleared, then stops and closes the stream
            # itself.  This eliminates the use-after-close race: the main thread
            # never touches an open stream, so join() timeout cannot cause a
            # read/close collision.
            try:
                while recording.is_set():
                    frames.append(stream.read(self.CHUNK, exception_on_overflow=False))
            finally:
                stream.stop_stream()
                stream.close()

        thread = threading.Thread(target=_capture, daemon=True)
        thread.start()

        print("  [Recording — press Enter to stop]", flush=True)
        input()

        # Signal the capture thread to stop.  The thread exits its read loop on
        # the next flag check and closes the stream in its own finally block.
        # join() gives it time to finish; if it times out the stream is either
        # already closed or the daemon thread will close it on exit — the main
        # thread never calls stream.close() so no double-close can occur.
        recording.clear()
        thread.join(timeout=3.0)  # one CHUNK at 16 kHz ≈ 64 ms; 3 s is generous

        return self._frames_to_wav(frames)

    def _frames_to_wav(self, frames: list[bytes]) -> bytes:
        """Convert raw PCM frames list to in-memory WAV bytes.

        Why: Deepgram pre-recorded API accepts WAV with the header intact.
        What: Writes frames into an in-memory BytesIO WAV file and returns bytes.
        Test: Pass known PCM frames; assert output starts with b'RIFF'.
        """
        buf = BytesIO()
        with wave.open(buf, "wb") as wf:
            wf.setnchannels(self.CHANNELS)
            wf.setsampwidth(self.SAMPLE_WIDTH)
            wf.setframerate(self.SAMPLE_RATE)
            wf.writeframes(b"".join(frames))
        return buf.getvalue()

    def close(self) -> None:
        """Release pyaudio resources.

        Why: Prevents resource-leak warnings at exit.
        What: Terminates PyAudio if initialised.
        Test: Call close() twice; assert no exception.
        """
        if self._pa is not None:
            self._pa.terminate()
            self._pa = None
