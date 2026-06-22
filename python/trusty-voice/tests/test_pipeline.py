"""
Tests for trusty_voice.pipeline.VoicePipeline (text mode).

All tests use text_mode=True with injected mock components so no audio
hardware, Deepgram key, or ElevenLabs key is required.

Coverage:
- run_once() sends the transcript to the daemon and returns reply text
- run_once() calls speaker.speak() with the reply
- run_once() updates conv_id from the daemon response
- run_once() handles DaemonError gracefully (returns error string, no raise)
- TextOnlySpeaker prints to stdout
- TextPassthroughTranscriber returns empty TranscriptResult

REAL-SDK AUDIO-MODE CHECK (regression guard):
- test_voice_pipeline_audio_mode_construction_no_import_error constructs
  VoicePipeline with text_mode=False (audio mode), with pyaudio and MicRecorder
  mocked but Deepgram + ElevenLabs SDK imports REAL.  This is the test that
  would have caught BOTH the ElevenLabs and Deepgram ImportError bugs before
  they reached production.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from trusty_voice.config import VoiceConfig
from trusty_voice.daemon_client import ChatResponse, DaemonError
from trusty_voice.pipeline import VoicePipeline
from trusty_voice.stt import TextPassthroughTranscriber, TranscriptResult
from trusty_voice.tts import TextOnlySpeaker

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_config(**kwargs: object) -> VoiceConfig:
    return VoiceConfig(
        deepgram_api_key="fake-dg",  # pragma: allowlist secret
        elevenlabs_api_key="fake-el",  # pragma: allowlist secret
        text_mode=True,
        **kwargs,  # type: ignore[arg-type]
    )


def _make_mock_daemon(reply: str = "Agent reply", conv_id: str = "sess-1") -> AsyncMock:
    daemon = AsyncMock()
    daemon.send_message = AsyncMock(return_value=ChatResponse(text=reply, conv_id=conv_id))
    daemon.aclose = AsyncMock()
    return daemon


def _make_mock_speaker() -> AsyncMock:
    speaker = AsyncMock()
    speaker.speak = AsyncMock()
    speaker.close = MagicMock()
    return speaker


# ---------------------------------------------------------------------------
# run_once — happy path
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_once_returns_reply_text() -> None:
    """run_once() returns the agent reply text."""
    daemon = _make_mock_daemon(reply="Hello from agent")
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    result = await pipeline.run_once(text_input="hi there")
    assert result == "Hello from agent"


@pytest.mark.asyncio
async def test_run_once_calls_daemon_send_message() -> None:
    """run_once() forwards the input text to daemon.send_message."""
    daemon = _make_mock_daemon()
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    await pipeline.run_once(text_input="tell me about Python")
    daemon.send_message.assert_called_once_with(text="tell me about Python", conv_id=None)


@pytest.mark.asyncio
async def test_run_once_calls_speaker_speak() -> None:
    """run_once() calls speaker.speak() with the agent reply."""
    daemon = _make_mock_daemon(reply="I am a reply")
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    await pipeline.run_once(text_input="q")
    speaker.speak.assert_called_once_with("I am a reply")


@pytest.mark.asyncio
async def test_run_once_updates_conv_id() -> None:
    """run_once() retains the conv_id returned by the daemon for future turns."""
    daemon = _make_mock_daemon(reply="Hi", conv_id="new-sess")
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    await pipeline.run_once(text_input="first message")
    # Second call should forward the updated conv_id
    await pipeline.run_once(text_input="second message")
    second_call = daemon.send_message.call_args_list[1]
    assert second_call.kwargs["conv_id"] == "new-sess"


@pytest.mark.asyncio
async def test_run_once_with_initial_conv_id() -> None:
    """run_once() forwards a pre-configured conv_id from VoiceConfig."""
    daemon = _make_mock_daemon(conv_id="preset-sess")
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(
        config=_make_config(conv_id="preset-sess"), daemon=daemon, speaker=speaker
    )
    await pipeline.run_once(text_input="msg")
    daemon.send_message.assert_called_once_with(text="msg", conv_id="preset-sess")


# ---------------------------------------------------------------------------
# run_once — error handling
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_once_daemon_error_returns_error_string() -> None:
    """run_once() returns an error string (not raising) when daemon fails."""
    daemon = AsyncMock()
    daemon.send_message = AsyncMock(side_effect=DaemonError("connection refused"))
    daemon.aclose = AsyncMock()
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    result = await pipeline.run_once(text_input="hello")
    assert "[Error:" in result
    assert "connection refused" in result


@pytest.mark.asyncio
async def test_run_once_daemon_error_still_calls_speaker() -> None:
    """run_once() still calls speaker.speak() with the error message."""
    daemon = AsyncMock()
    daemon.send_message = AsyncMock(side_effect=DaemonError("timeout"))
    daemon.aclose = AsyncMock()
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    await pipeline.run_once(text_input="test")
    speaker.speak.assert_called_once()
    spoken = speaker.speak.call_args.args[0]
    assert "[Error:" in spoken


# ---------------------------------------------------------------------------
# Text mode guard
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_once_text_mode_requires_text_input() -> None:
    """run_once() in text_mode raises ValueError if text_input is None."""
    daemon = _make_mock_daemon()
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    with pytest.raises(ValueError, match="text_input"):
        await pipeline.run_once(text_input=None)


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
    speaker.close()  # should not raise


# ---------------------------------------------------------------------------
# TextPassthroughTranscriber
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_text_passthrough_transcriber_returns_empty() -> None:
    """TextPassthroughTranscriber.transcribe_bytes() returns empty transcript."""
    t = TextPassthroughTranscriber()
    result = await t.transcribe_bytes(b"any audio")
    assert isinstance(result, TranscriptResult)
    assert result.text == ""
    assert result.confidence == 1.0


# ---------------------------------------------------------------------------
# close()
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_pipeline_close() -> None:
    """pipeline.close() calls aclose on daemon and close on speaker."""
    daemon = _make_mock_daemon()
    speaker = _make_mock_speaker()
    pipeline = VoicePipeline(config=_make_config(), daemon=daemon, speaker=speaker)
    await pipeline.close()
    daemon.aclose.assert_called_once()
    speaker.close.assert_called_once()


# ---------------------------------------------------------------------------
# Real-SDK audio-mode construction guard
# ---------------------------------------------------------------------------


def test_voice_pipeline_audio_mode_construction_no_import_error() -> None:
    """VoicePipeline(text_mode=False) constructs without ImportError using real SDKs.

    Why: Both the ElevenLabs and Deepgram ImportErrors only manifested in audio
         mode (text_mode=False) because the real SDK classes are only constructed
         in that branch.  The text-mode tests (all above) never exercised those
         imports.  This test constructs the pipeline in audio mode so that any
         future SDK API change will fail here, not at runtime during a live demo.
    What: Patches pyaudio.PyAudio and trusty_voice.audio.MicRecorder to avoid
         hardware access.  Does NOT mock deepgram or elevenlabs — their imports
         must succeed against the REAL installed packages.  Asserts construction
         raises no ImportError.
    Test: Run without audio hardware.  Fails if deepgram or elevenlabs API drifts.
    """
    mock_pa = MagicMock()
    mock_mic = MagicMock()

    with (
        patch("pyaudio.PyAudio", return_value=mock_pa),
        patch("trusty_voice.audio.MicRecorder", return_value=mock_mic),
    ):
        config = VoiceConfig(
            deepgram_api_key="dummy-dg",  # pragma: allowlist secret
            elevenlabs_api_key="dummy-el",  # pragma: allowlist secret
            text_mode=False,
        )
        # This MUST NOT raise ImportError — if either SDK's API has drifted,
        # DeepgramTranscriber.__init__ or ElevenLabsSpeaker._get_el_client()
        # (called lazily) will fail here.
        try:
            pipeline = VoicePipeline(config=config)
        except ImportError as exc:
            raise AssertionError(
                f"VoicePipeline(text_mode=False) raised ImportError — SDK API has drifted: {exc}"
            ) from exc

        # Verify both components were wired up
        assert pipeline._transcriber is not None
        assert pipeline._speaker is not None
