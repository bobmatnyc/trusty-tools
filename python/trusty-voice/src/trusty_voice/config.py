"""
Configuration for trusty-voice Phase 0.

Why: Centralises all runtime configuration so key names, defaults, and env
     variable names live in one place and are easy to find or override in tests.
What: Reads environment variables (populated from .env.local via dotenv in
     __main__), validates required secrets, and exposes a frozen dataclass with
     sane defaults.
Test: Instantiate with explicit kwargs; assert attribute values. For env-var
     path, mock os.environ before calling VoiceConfig.from_env().
"""

from __future__ import annotations

import os
from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class VoiceConfig:
    """Frozen runtime configuration for trusty-voice.

    Why: A single immutable config object is easy to pass through the pipeline
         and deterministic to test — no mutable-default pitfalls.
    What: Holds all credentials and tunable parameters needed to run the voice
         loop.
    Test: Construct directly with test values; assert each field.
    """

    # Credentials
    deepgram_api_key: str
    elevenlabs_api_key: str

    # Daemon
    daemon_base_url: str = "http://127.0.0.1:7880"

    # STT
    stt_language: str = "en-US"
    stt_model: str = "nova-2"

    # TTS
    tts_voice_id: str = "21m00Tcm4TlvDq8ikWAM"  # ElevenLabs "Rachel"
    tts_model_id: str = "eleven_turbo_v2"

    # Session
    conv_id: str | None = None  # None → new session per run

    # Dry-run / text mode
    text_mode: bool = False

    @classmethod
    def from_env(cls) -> VoiceConfig:
        """Build config from environment variables.

        Why: Keeps credential loading in one place; callers don't scatter
             os.getenv() calls across the codebase.
        What: Reads DEEPGRAM_API_KEY and ELEVENLABS_API_KEY (required), plus
             optional overrides for URL, voices, and models.
        Test: Set os.environ before calling; assert returned config fields.
              Pass missing key to verify ValueError is raised.
        """
        deepgram_key = os.environ.get("DEEPGRAM_API_KEY", "")
        elevenlabs_key = os.environ.get("ELEVENLABS_API_KEY", "")

        missing: list[str] = []
        if not deepgram_key:
            missing.append("DEEPGRAM_API_KEY")
        if not elevenlabs_key:
            missing.append("ELEVENLABS_API_KEY")
        if missing:
            raise ValueError(
                f"Required environment variable(s) not set: {', '.join(missing)}. "
                "Copy .env.local from repo root and fill in your keys."
            )

        return cls(
            deepgram_api_key=deepgram_key,
            elevenlabs_api_key=elevenlabs_key,
            daemon_base_url=os.environ.get("TRUSTY_VOICE_DAEMON_URL", "http://127.0.0.1:7880"),
            stt_language=os.environ.get("TRUSTY_VOICE_STT_LANGUAGE", "en-US"),
            stt_model=os.environ.get("TRUSTY_VOICE_STT_MODEL", "nova-2"),
            tts_voice_id=os.environ.get("TRUSTY_VOICE_TTS_VOICE_ID", "21m00Tcm4TlvDq8ikWAM"),
            tts_model_id=os.environ.get("TRUSTY_VOICE_TTS_MODEL_ID", "eleven_turbo_v2"),
            conv_id=os.environ.get("TRUSTY_VOICE_CONV_ID") or None,
        )

    def redacted(self) -> dict[str, object]:
        """Return a loggable dict with secrets masked.

        Why: We need to log the active config for debugging without leaking
             API keys to stdout/stderr.
        What: Returns every field, replacing key values with '***'.
        Test: Assert returned dict has '***' for key fields, not the real value.
        """
        return {
            "deepgram_api_key": "***" if self.deepgram_api_key else "(unset)",
            "elevenlabs_api_key": "***" if self.elevenlabs_api_key else "(unset)",
            "daemon_base_url": self.daemon_base_url,
            "stt_language": self.stt_language,
            "stt_model": self.stt_model,
            "tts_voice_id": self.tts_voice_id,
            "tts_model_id": self.tts_model_id,
            "conv_id": self.conv_id,
            "text_mode": self.text_mode,
        }
