"""
Tests for trusty_voice.stt.DeepgramTranscriber.

Coverage:
- REAL-SDK IMPORT CHECK: test_deepgram_real_sdk_import verifies the ACTUALLY
  INSTALLED deepgram package exposes AsyncDeepgramClient with the v7 API surface
  (listen.v1.media.transcribe_file).  NOT mocked — fails loudly on API drift.
- DeepgramTranscriber constructs without network when _client is injected.
- DeepgramTranscriber._get_client() creates AsyncDeepgramClient from the real SDK.
- transcribe_bytes() calls transcribe_file with correct kwargs and extracts text.
- transcribe_bytes() handles missing/empty transcript gracefully.
- TextPassthroughTranscriber returns empty TranscriptResult.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from trusty_voice.stt import DeepgramTranscriber, TextPassthroughTranscriber, TranscriptResult

# ---------------------------------------------------------------------------
# Real-SDK import check (non-mocked; regression guard)
# ---------------------------------------------------------------------------


def test_deepgram_real_sdk_import() -> None:
    """Verify the INSTALLED deepgram SDK exposes the v7 API surface.

    Why: The original code used PrerecordedOptions (removed in v4+) and passed
         api_key positionally (broke in v7).  This test exercises the REAL installed
         package (no mocking) so that any future SDK change is caught immediately,
         not at runtime when the live audio demo crashes.
    What: Imports AsyncDeepgramClient; constructs with api_key= keyword (no network);
         asserts listen.v1.media.transcribe_file is callable.
    Test: This test IS the check — if it passes, the v7 SDK surface is intact.
    """
    # Must use the real module — no monkeypatching allowed in this test.
    from deepgram import AsyncDeepgramClient  # type: ignore[import]

    client = AsyncDeepgramClient(api_key="dummy-key-no-network")  # pragma: allowlist secret
    assert hasattr(client, "listen"), "AsyncDeepgramClient has no .listen — SDK API changed"
    assert hasattr(client.listen, "v1"), (
        "AsyncDeepgramClient.listen has no .v1 — SDK API changed; "
        "check client.listen attributes for the new pre-recorded entrypoint"
    )
    assert hasattr(client.listen.v1, "media"), (
        "AsyncDeepgramClient.listen.v1 has no .media — SDK API changed"
    )
    assert callable(getattr(client.listen.v1.media, "transcribe_file", None)), (
        "AsyncDeepgramClient.listen.v1.media.transcribe_file is not callable — SDK API changed"
    )


def test_deepgram_transcriber_uses_real_sdk_client() -> None:
    """DeepgramTranscriber._get_client() creates AsyncDeepgramClient from the real SDK.

    Why: Confirms the lazy-init path resolves against the installed package
         without triggering network I/O.
    What: Calls _get_client() on a fresh transcriber (no injected _client);
         asserts the result type is AsyncDeepgramClient from the real SDK.
    Test: Run; no mock; no network.  Passes iff SDK import and constructor work.
    """
    from deepgram import AsyncDeepgramClient  # type: ignore[import]

    t = DeepgramTranscriber(api_key="dummy")  # pragma: allowlist secret
    client = t._get_client()
    assert isinstance(client, AsyncDeepgramClient)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_mock_response(transcript: str = "hello world", confidence: float = 0.95) -> MagicMock:
    """Build a mock ListenV1Response matching the v7 SDK shape."""
    alt = MagicMock()
    alt.transcript = transcript
    alt.confidence = confidence

    channel = MagicMock()
    channel.alternatives = [alt]

    result = MagicMock()
    result.channels = [channel]

    response = MagicMock()
    response.results = result
    response.model_dump.return_value = {"results": {"channels": []}}
    return response


def _make_mock_dg_client(transcript: str = "hello world") -> MagicMock:
    """Return a mock AsyncDeepgramClient whose listen.v1.media.transcribe_file is async."""
    mock_response = _make_mock_response(transcript=transcript)
    mock_media = MagicMock()
    mock_media.transcribe_file = AsyncMock(return_value=mock_response)

    mock_v1 = MagicMock()
    mock_v1.media = mock_media

    mock_listen = MagicMock()
    mock_listen.v1 = mock_v1

    mock_client = MagicMock()
    mock_client.listen = mock_listen
    return mock_client


# ---------------------------------------------------------------------------
# Construction
# ---------------------------------------------------------------------------


def test_deepgram_transcriber_construction_with_injected_client() -> None:
    """DeepgramTranscriber constructs without network when _client is injected."""
    mock_client = MagicMock()
    t = DeepgramTranscriber(api_key="x", _client=mock_client)  # pragma: allowlist secret
    assert t._get_client() is mock_client


# ---------------------------------------------------------------------------
# transcribe_bytes() — happy path
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_transcribe_bytes_returns_transcript_text() -> None:
    """transcribe_bytes() extracts transcript text from the mock response."""
    mock_client = _make_mock_dg_client(transcript="test transcript")
    t = DeepgramTranscriber(api_key="x", _client=mock_client)  # pragma: allowlist secret
    result = await t.transcribe_bytes(b"\x00\x01\x02\x03")
    assert isinstance(result, TranscriptResult)
    assert result.text == "test transcript"


@pytest.mark.asyncio
async def test_transcribe_bytes_calls_transcribe_file_with_correct_kwargs() -> None:
    """transcribe_bytes() passes request= bytes and option kwargs to transcribe_file."""
    mock_client = _make_mock_dg_client()
    t = DeepgramTranscriber(
        api_key="x",  # pragma: allowlist secret
        language="fr-FR",
        model="nova-3",
        _client=mock_client,
    )
    audio = b"\xff" * 512
    await t.transcribe_bytes(audio)

    mock_client.listen.v1.media.transcribe_file.assert_called_once_with(
        request=audio,
        model="nova-3",
        language="fr-FR",
        smart_format=True,
        punctuate=True,
    )


@pytest.mark.asyncio
async def test_transcribe_bytes_extracts_confidence() -> None:
    """transcribe_bytes() extracts confidence from the alternative."""
    mock_response = _make_mock_response(transcript="hello", confidence=0.88)
    mock_client = _make_mock_dg_client()
    mock_client.listen.v1.media.transcribe_file = AsyncMock(return_value=mock_response)
    t = DeepgramTranscriber(api_key="x", _client=mock_client)  # pragma: allowlist secret
    result = await t.transcribe_bytes(b"\x00")
    assert abs(result.confidence - 0.88) < 1e-6


# ---------------------------------------------------------------------------
# transcribe_bytes() — error handling
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_transcribe_bytes_handles_empty_transcript() -> None:
    """transcribe_bytes() returns empty text when SDK returns empty string."""
    mock_client = _make_mock_dg_client(transcript="")
    t = DeepgramTranscriber(api_key="x", _client=mock_client)  # pragma: allowlist secret
    result = await t.transcribe_bytes(b"\x00")
    assert result.text == ""


@pytest.mark.asyncio
async def test_transcribe_bytes_handles_missing_results() -> None:
    """transcribe_bytes() returns empty text when response has no channels."""
    broken_response = MagicMock()
    broken_response.results.channels = []
    broken_response.model_dump.return_value = {}

    mock_client = MagicMock()
    mock_client.listen.v1.media.transcribe_file = AsyncMock(return_value=broken_response)

    t = DeepgramTranscriber(api_key="x", _client=mock_client)  # pragma: allowlist secret
    result = await t.transcribe_bytes(b"\x00")
    assert result.text == ""
    assert result.confidence == 0.0


# ---------------------------------------------------------------------------
# TextPassthroughTranscriber
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_text_passthrough_returns_empty_transcript() -> None:
    """TextPassthroughTranscriber.transcribe_bytes() returns empty TranscriptResult."""
    t = TextPassthroughTranscriber()
    result = await t.transcribe_bytes(b"any audio")
    assert isinstance(result, TranscriptResult)
    assert result.text == ""
    assert result.confidence == 1.0
