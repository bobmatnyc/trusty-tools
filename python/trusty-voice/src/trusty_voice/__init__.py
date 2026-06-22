"""
trusty-voice: Phase 0 push-to-talk voice interface for trusty-mpm agents.

Why: Provides a voice loop (mic → STT → agent → TTS → speaker) that lets a
     developer speak to the local trusty-mpm coding agent without typing.
What: Package root — re-exports the public API surface used by the CLI entry
     point and external tooling.
Test: Import this module; no exceptions should be raised.  The sub-modules
     carry their own unit tests in tests/.
"""

from trusty_voice.config import VoiceConfig
from trusty_voice.version import __version__

__all__ = ["VoiceConfig", "__version__"]
