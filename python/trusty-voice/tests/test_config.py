"""
Tests for trusty_voice.config.

Coverage:
- VoiceConfig construction with explicit values
- VoiceConfig.from_env() happy path
- VoiceConfig.from_env() raises ValueError on missing keys
- VoiceConfig.redacted() masks secrets
"""

from __future__ import annotations

import pytest

from trusty_voice.config import VoiceConfig

# ---------------------------------------------------------------------------
# Direct construction
# ---------------------------------------------------------------------------


def test_voiceconfig_defaults() -> None:
    """VoiceConfig stores all required fields and applies sane defaults."""
    cfg = VoiceConfig(
        deepgram_api_key="dg-test",  # pragma: allowlist secret
        elevenlabs_api_key="el-test",  # pragma: allowlist secret
    )
    assert cfg.deepgram_api_key == "dg-test"  # pragma: allowlist secret
    assert cfg.elevenlabs_api_key == "el-test"  # pragma: allowlist secret
    assert cfg.daemon_base_url == "http://127.0.0.1:7880"
    assert cfg.stt_language == "en-US"
    assert cfg.stt_model == "nova-2"
    assert cfg.tts_model_id == "eleven_turbo_v2"
    assert cfg.conv_id is None
    assert cfg.text_mode is False


def test_voiceconfig_custom_values() -> None:
    """Custom values are stored verbatim."""
    cfg = VoiceConfig(
        deepgram_api_key="dg",  # pragma: allowlist secret
        elevenlabs_api_key="el",  # pragma: allowlist secret
        daemon_base_url="http://localhost:9999",
        tts_voice_id="custom-voice",
        conv_id="abc-123",
        text_mode=True,
    )
    assert cfg.daemon_base_url == "http://localhost:9999"
    assert cfg.tts_voice_id == "custom-voice"
    assert cfg.conv_id == "abc-123"
    assert cfg.text_mode is True


def test_voiceconfig_frozen() -> None:
    """VoiceConfig is immutable (frozen dataclass)."""
    cfg = VoiceConfig(deepgram_api_key="dg", elevenlabs_api_key="el")  # pragma: allowlist secret
    with pytest.raises((TypeError, AttributeError)):  # FrozenInstanceError
        cfg.deepgram_api_key = "changed"  # type: ignore[misc]


# ---------------------------------------------------------------------------
# from_env()
# ---------------------------------------------------------------------------


def test_from_env_happy_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() builds a config when both keys are in the environment."""
    monkeypatch.setenv("DEEPGRAM_API_KEY", "dg-key")
    monkeypatch.setenv("ELEVENLABS_API_KEY", "el-key")
    monkeypatch.delenv("TRUSTY_VOICE_DAEMON_URL", raising=False)

    cfg = VoiceConfig.from_env()
    assert cfg.deepgram_api_key == "dg-key"  # pragma: allowlist secret
    assert cfg.elevenlabs_api_key == "el-key"  # pragma: allowlist secret
    assert cfg.daemon_base_url == "http://127.0.0.1:7880"


def test_from_env_custom_url(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() picks up TRUSTY_VOICE_DAEMON_URL override."""
    monkeypatch.setenv("DEEPGRAM_API_KEY", "dg")
    monkeypatch.setenv("ELEVENLABS_API_KEY", "el")
    monkeypatch.setenv("TRUSTY_VOICE_DAEMON_URL", "http://192.168.1.5:7880")

    cfg = VoiceConfig.from_env()
    assert cfg.daemon_base_url == "http://192.168.1.5:7880"


def test_from_env_missing_both_keys(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() raises ValueError when both API keys are absent."""
    monkeypatch.delenv("DEEPGRAM_API_KEY", raising=False)
    monkeypatch.delenv("ELEVENLABS_API_KEY", raising=False)

    with pytest.raises(ValueError, match="DEEPGRAM_API_KEY"):
        VoiceConfig.from_env()


def test_from_env_missing_one_key(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() raises ValueError naming the missing key."""
    monkeypatch.setenv("DEEPGRAM_API_KEY", "dg")
    monkeypatch.delenv("ELEVENLABS_API_KEY", raising=False)

    with pytest.raises(ValueError, match="ELEVENLABS_API_KEY"):
        VoiceConfig.from_env()


def test_from_env_conv_id(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() reads TRUSTY_VOICE_CONV_ID when set."""
    monkeypatch.setenv("DEEPGRAM_API_KEY", "dg")
    monkeypatch.setenv("ELEVENLABS_API_KEY", "el")
    monkeypatch.setenv("TRUSTY_VOICE_CONV_ID", "session-42")

    cfg = VoiceConfig.from_env()
    assert cfg.conv_id == "session-42"


def test_from_env_empty_conv_id_treated_as_none(monkeypatch: pytest.MonkeyPatch) -> None:
    """from_env() converts empty TRUSTY_VOICE_CONV_ID to None."""
    monkeypatch.setenv("DEEPGRAM_API_KEY", "dg")
    monkeypatch.setenv("ELEVENLABS_API_KEY", "el")
    monkeypatch.setenv("TRUSTY_VOICE_CONV_ID", "")

    cfg = VoiceConfig.from_env()
    assert cfg.conv_id is None


# ---------------------------------------------------------------------------
# redacted()
# ---------------------------------------------------------------------------


def test_redacted_masks_keys() -> None:
    """redacted() returns '***' for both API keys."""
    cfg = VoiceConfig(deepgram_api_key="real-dg-key", elevenlabs_api_key="real-el-key")
    r = cfg.redacted()
    assert r["deepgram_api_key"] == "***"
    assert r["elevenlabs_api_key"] == "***"
    assert "real-dg-key" not in str(r)
    assert "real-el-key" not in str(r)


def test_redacted_exposes_non_secret_fields() -> None:
    """redacted() does expose non-secret config fields."""
    cfg = VoiceConfig(
        deepgram_api_key="dg",  # pragma: allowlist secret
        elevenlabs_api_key="el",  # pragma: allowlist secret
        daemon_base_url="http://localhost:9999",
    )
    r = cfg.redacted()
    assert r["daemon_base_url"] == "http://localhost:9999"


def test_redacted_unset_keys_show_marker() -> None:
    """redacted() shows '(unset)' when a key is empty string."""
    cfg = VoiceConfig(deepgram_api_key="", elevenlabs_api_key="")
    r = cfg.redacted()
    assert r["deepgram_api_key"] == "(unset)"
    assert r["elevenlabs_api_key"] == "(unset)"
