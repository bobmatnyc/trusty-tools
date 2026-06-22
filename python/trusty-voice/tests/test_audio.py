"""
Tests for trusty_voice.audio.MicRecorder (non-hardware paths).

We don't open a real microphone; pyaudio is monkeypatched so all
stream/read calls are intercepted.

Coverage:
- _frames_to_wav() returns valid WAV bytes
- record_for_seconds() calls read() the expected number of times
- close() is idempotent
"""

from __future__ import annotations

import wave
from io import BytesIO
from unittest.mock import MagicMock, patch

from trusty_voice.audio import MicRecorder

# ---------------------------------------------------------------------------
# _frames_to_wav
# ---------------------------------------------------------------------------


def test_frames_to_wav_returns_valid_wav() -> None:
    """_frames_to_wav() produces bytes that can be parsed by the wave module."""
    recorder = MicRecorder()
    # 100 ms of silence at 16 kHz / 16-bit mono = 1600 * 2 = 3200 bytes
    silence = b"\x00" * 3200
    wav_bytes = recorder._frames_to_wav([silence])

    assert wav_bytes[:4] == b"RIFF"
    buf = BytesIO(wav_bytes)
    with wave.open(buf, "rb") as wf:
        assert wf.getnchannels() == 1
        assert wf.getsampwidth() == 2
        assert wf.getframerate() == 16000


def test_frames_to_wav_empty_frames() -> None:
    """_frames_to_wav([]) produces a valid (silent) WAV file."""
    recorder = MicRecorder()
    wav_bytes = recorder._frames_to_wav([])
    assert wav_bytes[:4] == b"RIFF"


def test_frames_to_wav_multiple_chunks() -> None:
    """_frames_to_wav() concatenates multiple frame chunks."""
    recorder = MicRecorder()
    chunk = b"\x01\x02" * 512
    wav_bytes = recorder._frames_to_wav([chunk, chunk])
    buf = BytesIO(wav_bytes)
    with wave.open(buf, "rb") as wf:
        # 2 * 512 * 2 = 2048 sample bytes → 1024 frames at 16-bit
        assert wf.getnframes() == 1024


# ---------------------------------------------------------------------------
# record_for_seconds (mocked pyaudio)
# ---------------------------------------------------------------------------


def _make_mock_pyaudio(chunk_data: bytes = b"\x00" * 1024) -> MagicMock:
    """Return a mock pyaudio module with a stubbed PyAudio and stream."""
    mock_stream = MagicMock()
    mock_stream.read.return_value = chunk_data

    mock_pa = MagicMock()
    mock_pa.open.return_value = mock_stream
    mock_pa.paInt16 = 8  # numeric constant doesn't matter for mocks

    mock_module = MagicMock()
    mock_module.PyAudio.return_value = mock_pa
    mock_module.paInt16 = 8

    return mock_module


def test_record_for_seconds_reads_correct_chunk_count() -> None:
    """record_for_seconds(0.1) reads ceil(0.1 * 16000 / 1024) = 2 chunks."""
    import math

    mock_pyaudio_module = _make_mock_pyaudio()

    with patch.dict("sys.modules", {"pyaudio": mock_pyaudio_module}):
        recorder = MicRecorder()
        recorder._pa = mock_pyaudio_module.PyAudio()
        wav_bytes = recorder.record_for_seconds(0.1)

    expected_chunks = math.ceil(0.1 * MicRecorder.SAMPLE_RATE / MicRecorder.CHUNK)
    stream = recorder._pa.open.return_value
    assert stream.read.call_count == expected_chunks
    assert wav_bytes[:4] == b"RIFF"


def test_record_for_seconds_closes_stream_on_success() -> None:
    """record_for_seconds() calls stop_stream() and close() even on success."""
    mock_pyaudio_module = _make_mock_pyaudio()

    with patch.dict("sys.modules", {"pyaudio": mock_pyaudio_module}):
        recorder = MicRecorder()
        recorder._pa = mock_pyaudio_module.PyAudio()
        recorder.record_for_seconds(0.05)

    stream = recorder._pa.open.return_value
    stream.stop_stream.assert_called_once()
    stream.close.assert_called_once()


# ---------------------------------------------------------------------------
# record_blocking — stream owned by capture thread (use-after-close guard)
# ---------------------------------------------------------------------------


def test_record_blocking_capture_thread_closes_stream() -> None:
    """record_blocking() capture thread owns stop/close so no use-after-close race.

    Why: The previous implementation closed the stream in the main thread after
         join(), which could still race if join() timed out.  The fix moves
         stop_stream()+close() inside the capture thread's finally block.
    What: Simulates a complete push-to-talk cycle using mock pyaudio and a
         patched input() that returns immediately (simulating Enter being pressed).
         Asserts that stop_stream() and close() are called by the thread (i.e.,
         they ARE called) and that the main thread does NOT call them separately
         (only once total each).
    Test: Assert stop_stream and close each called exactly once (by the thread).
    """

    mock_pyaudio_module = _make_mock_pyaudio()
    mock_pa = mock_pyaudio_module.PyAudio()
    mock_stream = mock_pa.open.return_value

    # Make read() return instantly (no blocking) so the thread loop is fast.
    mock_stream.read.return_value = b"\x00" * MicRecorder.CHUNK

    recorder = MicRecorder()
    recorder._pa = mock_pa

    # Patch input() so the "press Enter" call returns immediately.
    with (
        patch.dict("sys.modules", {"pyaudio": mock_pyaudio_module}),
        patch("builtins.input", return_value=""),
    ):
        wav_bytes = recorder.record_blocking()

    # The capture thread's finally block must have called stop + close.
    mock_stream.stop_stream.assert_called_once()
    mock_stream.close.assert_called_once()

    # The returned bytes must be a valid WAV file.
    assert wav_bytes[:4] == b"RIFF"


# ---------------------------------------------------------------------------
# close() idempotent
# ---------------------------------------------------------------------------


def test_close_idempotent_without_pa() -> None:
    """close() on a recorder that never opened pyaudio raises no exception."""
    recorder = MicRecorder()
    recorder.close()  # first call — _pa is None
    recorder.close()  # second call — still None


def test_close_terminates_pa() -> None:
    """close() calls terminate() on the PyAudio instance."""
    mock_pa = MagicMock()
    recorder = MicRecorder()
    recorder._pa = mock_pa

    recorder.close()

    mock_pa.terminate.assert_called_once()
    assert recorder._pa is None
