"""
Tests for trusty_voice.tts.

Coverage:
- ElevenLabsSpeaker constructs without touching audio hardware or network
- ElevenLabsSpeaker.speak() feeds async PCM chunks to pyaudio stream.write()
- ElevenLabsSpeaker.speak() skips empty text
- ElevenLabsSpeaker.close() is idempotent
- TextOnlySpeaker.speak() prints [TTS] prefix
- TextOnlySpeaker.close() is a no-op

REAL-SDK CHECK (Finding 1 regression guard):
- test_elevenlabs_real_sdk_import verifies that the ACTUALLY INSTALLED elevenlabs
  package exposes AsyncElevenLabs with a text_to_speech.stream method.
  This is NOT mocked — if the ElevenLabs SDK changes its API surface, this
  test will fail loudly before any audio path does.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from unittest.mock import MagicMock

import pytest

from trusty_voice.tts import ElevenLabsSpeaker, TextOnlySpeaker

# ---------------------------------------------------------------------------
# Real-SDK import check (non-mocked; regression guard for Finding 1)
# ---------------------------------------------------------------------------


def test_elevenlabs_real_sdk_import() -> None:
    """Verify the INSTALLED elevenlabs SDK exposes the ≥1.x client API surface.

    Why: The original implementation used the pre-1.0 module-level generate() /
         set_api_key() helpers which were REMOVED in elevenlabs ≥1.0.  This test
         exercises the REAL installed package (no mocking) so that an API-breaking
         downgrade or future SDK change is caught immediately, not at runtime.
    What: Imports AsyncElevenLabs; constructs a client with a dummy key (no network
         call); asserts .text_to_speech exists and .text_to_speech.stream is callable.
    Test: This test IS the check — if it passes, the SDK surface is intact.
    """
    # Must import the real module — no monkeypatching allowed in this test.
    from elevenlabs.client import AsyncElevenLabs  # type: ignore[import]

    client = AsyncElevenLabs(api_key="dummy-key-no-network")  # pragma: allowlist secret
    assert hasattr(client, "text_to_speech"), (
        "AsyncElevenLabs has no .text_to_speech — SDK API changed"
    )
    assert callable(getattr(client.text_to_speech, "stream", None)), (
        "AsyncElevenLabs.text_to_speech.stream is not callable — SDK API changed"
    )
    assert callable(getattr(client.text_to_speech, "convert", None)), (
        "AsyncElevenLabs.text_to_speech.convert is not callable — SDK API changed"
    )


def test_elevenlabs_speaker_construction_uses_real_sdk() -> None:
    """ElevenLabsSpeaker._get_el_client() creates AsyncElevenLabs from the real SDK.

    Why: Confirms the constructor path resolves against the installed package
         without triggering audio or network I/O.
    What: Calls _get_el_client() on a fresh speaker and asserts the client type
         is AsyncElevenLabs from the installed SDK.
    Test: Run; no mock; no network.  Passes iff SDK import works.
    """
    from elevenlabs.client import AsyncElevenLabs  # type: ignore[import]

    speaker = ElevenLabsSpeaker(api_key="dummy")  # pragma: allowlist secret
    client = speaker._get_el_client()
    assert isinstance(client, AsyncElevenLabs)


# ---------------------------------------------------------------------------
# speak() with injected mock client (no network, no audio hardware)
# ---------------------------------------------------------------------------


async def _async_chunk_gen(*chunks: bytes) -> AsyncIterator[bytes]:
    """Async generator yielding the given byte chunks."""
    for chunk in chunks:
        yield chunk


def _make_mock_elevenlabs_client(chunks: list[bytes]) -> MagicMock:
    """Return a mock AsyncElevenLabs client whose stream() yields chunks."""
    mock_client = MagicMock()
    mock_client.text_to_speech.stream = MagicMock(return_value=_async_chunk_gen(*chunks))
    return mock_client


def _make_mock_pyaudio_stream() -> tuple[MagicMock, MagicMock]:
    """Return (mock_pa, mock_stream) with stream.write() stubbed."""
    mock_stream = MagicMock()
    mock_pa = MagicMock()
    mock_pa.open.return_value = mock_stream
    mock_pa.paInt16 = 8
    return mock_pa, mock_stream


@pytest.mark.asyncio
async def test_speak_sends_chunks_to_pyaudio() -> None:
    """speak() forwards each PCM chunk from the ElevenLabs async iterator to pyaudio."""
    chunks = [b"\x00\x01" * 512, b"\x02\x03" * 512]
    mock_client = _make_mock_elevenlabs_client(chunks)
    mock_pa, mock_stream = _make_mock_pyaudio_stream()

    speaker = ElevenLabsSpeaker(
        api_key="dummy",  # pragma: allowlist secret
        voice_id="voice-123",
        model_id="model-abc",
        _client=mock_client,
    )
    speaker._pa = mock_pa

    await speaker.speak("Hello world")

    mock_client.text_to_speech.stream.assert_called_once_with(
        voice_id="voice-123",
        text="Hello world",
        model_id="model-abc",
        output_format="pcm_22050",
    )
    assert mock_stream.write.call_count == len(chunks)
    mock_stream.write.assert_any_call(chunks[0])
    mock_stream.write.assert_any_call(chunks[1])


@pytest.mark.asyncio
async def test_speak_closes_stream_always() -> None:
    """speak() calls stop_stream() and close() even when chunks raise an error."""

    async def _error_gen() -> AsyncIterator[bytes]:
        yield b"\x00" * 512
        raise RuntimeError("SDK error mid-stream")

    mock_client = MagicMock()
    mock_client.text_to_speech.stream = MagicMock(return_value=_error_gen())
    mock_pa, mock_stream = _make_mock_pyaudio_stream()

    speaker = ElevenLabsSpeaker(api_key="dummy", _client=mock_client)  # pragma: allowlist secret
    speaker._pa = mock_pa

    with pytest.raises(RuntimeError, match="SDK error mid-stream"):
        await speaker.speak("test")

    mock_stream.stop_stream.assert_called_once()
    mock_stream.close.assert_called_once()


@pytest.mark.asyncio
async def test_speak_skips_empty_text() -> None:
    """speak() returns immediately (no SDK call) when text is blank."""
    mock_client = _make_mock_elevenlabs_client([b"some audio"])
    mock_pa, _ = _make_mock_pyaudio_stream()

    speaker = ElevenLabsSpeaker(api_key="dummy", _client=mock_client)  # pragma: allowlist secret
    speaker._pa = mock_pa

    await speaker.speak("   ")  # whitespace only

    mock_client.text_to_speech.stream.assert_not_called()


@pytest.mark.asyncio
async def test_speak_skips_empty_chunks() -> None:
    """speak() skips empty bytes chunks (doesn't write them to pyaudio)."""
    chunks = [b"", b"\x01\x02" * 256, b""]
    mock_client = _make_mock_elevenlabs_client(chunks)
    mock_pa, mock_stream = _make_mock_pyaudio_stream()

    speaker = ElevenLabsSpeaker(api_key="dummy", _client=mock_client)  # pragma: allowlist secret
    speaker._pa = mock_pa

    await speaker.speak("hi")

    # Only the non-empty chunk should be written
    assert mock_stream.write.call_count == 1
    mock_stream.write.assert_called_once_with(b"\x01\x02" * 256)


# ---------------------------------------------------------------------------
# close()
# ---------------------------------------------------------------------------


def test_close_terminates_pyaudio() -> None:
    """close() calls terminate() on the PyAudio instance."""
    mock_pa = MagicMock()
    speaker = ElevenLabsSpeaker(api_key="dummy")  # pragma: allowlist secret
    speaker._pa = mock_pa
    speaker.close()
    mock_pa.terminate.assert_called_once()
    assert speaker._pa is None


def test_close_idempotent_without_pa() -> None:
    """close() on a fresh speaker (pyaudio never initialised) raises no exception."""
    speaker = ElevenLabsSpeaker(api_key="dummy")  # pragma: allowlist secret
    speaker.close()  # first call
    speaker.close()  # second call — no exception


# ---------------------------------------------------------------------------
# TextOnlySpeaker
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_text_only_speaker_prints(capsys: pytest.CaptureFixture[str]) -> None:
    """TextOnlySpeaker.speak() prints [TTS] prefix and the text to stdout."""
    speaker = TextOnlySpeaker()
    await speaker.speak("Hello world")
    captured = capsys.readouterr()
    assert "[TTS]" in captured.out
    assert "Hello world" in captured.out


@pytest.mark.asyncio
async def test_text_only_speaker_close_noop() -> None:
    """TextOnlySpeaker.close() succeeds without error."""
    speaker = TextOnlySpeaker()
    speaker.close()
