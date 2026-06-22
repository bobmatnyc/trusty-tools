"""
Push-to-talk voice pipeline (Phase 0).

Why: A single module that wires STT → daemon → TTS into a clean async loop
     keeps the entry point thin and makes each step independently testable.
What: Provides VoicePipeline — manages one full utterance cycle (record → STT
     → daemon chat → TTS) and a run_loop() that repeats until Ctrl-C.
Test: Inject mock STT, daemon client, and TTS speaker; call run_once() with a
     known transcript; assert daemon.send_message and speaker.speak were called
     with the expected strings.
"""

from __future__ import annotations

import asyncio
import logging

from trusty_voice.audio import MicRecorder
from trusty_voice.config import VoiceConfig
from trusty_voice.daemon_client import DaemonClient, DaemonError
from trusty_voice.stt import DeepgramTranscriber, TextPassthroughTranscriber, TranscriptResult
from trusty_voice.tts import ElevenLabsSpeaker, TextOnlySpeaker

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Pipeline
# ---------------------------------------------------------------------------


class VoicePipeline:
    """Orchestrates one push-to-talk utterance cycle.

    Why: Encapsulating the cycle in a class lets tests inject mocks and verify
         each step without running audio hardware.
    What: Holds references to the STT, daemon, and TTS components; run_once()
         executes one full cycle; run_loop() repeats until cancelled.
    Test: Inject mocks for all three components; call run_once("hello world");
          assert daemon.send_message("hello world") was called.
    """

    def __init__(
        self,
        config: VoiceConfig,
        transcriber: DeepgramTranscriber | TextPassthroughTranscriber | None = None,
        daemon: DaemonClient | None = None,
        speaker: ElevenLabsSpeaker | TextOnlySpeaker | None = None,
    ) -> None:
        """
        Why: Dependency injection lets tests replace any component without
             touching the pipeline logic.
        What: Stores config and components; creates defaults if not provided
             (except in text_mode, where lightweight stand-ins are used).
        Test: Pass mocks for all three; assert they are stored as-is.
        """
        self._config = config
        self._conv_id: str | None = config.conv_id

        # Allow full injection for testing
        if transcriber is not None:
            self._transcriber: DeepgramTranscriber | TextPassthroughTranscriber = transcriber
        elif config.text_mode:
            self._transcriber = TextPassthroughTranscriber()
        else:
            self._transcriber = DeepgramTranscriber(
                api_key=config.deepgram_api_key,
                language=config.stt_language,
                model=config.stt_model,
            )

        if daemon is not None:
            self._daemon: DaemonClient = daemon
        else:
            self._daemon = DaemonClient(base_url=config.daemon_base_url)

        if speaker is not None:
            self._speaker: ElevenLabsSpeaker | TextOnlySpeaker = speaker
        elif config.text_mode:
            self._speaker = TextOnlySpeaker()
        else:
            self._speaker = ElevenLabsSpeaker(
                api_key=config.elevenlabs_api_key,
                voice_id=config.tts_voice_id,
                model_id=config.tts_model_id,
            )

        self._recorder: MicRecorder | None = None if config.text_mode else MicRecorder()

    async def run_once(self, text_input: str | None = None) -> str:
        """Execute one utterance cycle; return the agent reply text.

        Why: Single-cycle method is testable in isolation without a full loop.
        What: In text_mode uses text_input; otherwise records mic audio,
              transcribes, sends to daemon, speaks reply.
        Test: Pass text_input="hello"; assert returned string matches mock reply.
        """
        if self._config.text_mode:
            if text_input is None:
                raise ValueError("text_input must be provided in text_mode")
            transcript = text_input
        else:
            # Mic capture
            assert self._recorder is not None
            audio_bytes = self._recorder.record_blocking()
            result: TranscriptResult = await self._transcriber.transcribe_bytes(audio_bytes)
            transcript = result.text.strip()
            if not transcript:
                logger.info("Empty transcript — skipping")
                return ""

        logger.info("You said: %r", transcript)
        print(f"\n[You] {transcript}")

        # Daemon call
        try:
            chat_resp = await self._daemon.send_message(text=transcript, conv_id=self._conv_id)
            self._conv_id = chat_resp.conv_id
            reply = chat_resp.text
        except DaemonError as exc:
            logger.error("Daemon error: %s", exc)
            reply = f"[Error: {exc}]"

        logger.info("Agent reply: %r", reply)
        print(f"[Agent] {reply}")

        # TTS playback
        await self._speaker.speak(reply)

        return reply

    async def run_loop(self) -> None:
        """Run push-to-talk loop until Ctrl-C.

        Why: The main interactive mode — each iteration waits for a new
             utterance and responds.
        What: Repeatedly calls run_once(); handles KeyboardInterrupt cleanly.
        Test: Run with asyncio.timeout(0.1) to exercise the cancel path;
              assert no unhandled exception.
        """
        print("\ntrustY voice — Phase 0 push-to-talk\nCtrl-C to quit.\n")
        while True:
            print("\n[Press Enter to start recording]", flush=True)
            try:
                await self.run_once()
            except (KeyboardInterrupt, asyncio.CancelledError):
                print("\n[Goodbye]")
                break
            except Exception as exc:
                logger.error("Unexpected error in pipeline loop: %s", exc, exc_info=True)
                print(f"[Error] {exc}")

    async def run_text_loop(self) -> None:
        """Interactive text-mode loop — type messages, read replies.

        Why: Validates the full daemon→TTS path without audio hardware.
        What: Reads stdin lines; sends to run_once(text_input=line); repeats.
        Test: Pipe input to the process; assert output contains [TTS] lines.
        """
        print(
            "\ntrustY voice — Phase 0 text mode (no audio)\n"
            "Type a message and press Enter. Ctrl-C or 'quit' to exit.\n"
        )
        while True:
            try:
                raw = await asyncio.get_running_loop().run_in_executor(
                    None, lambda: input("[You] > ")
                )
            except EOFError:
                break
            text = raw.strip()
            if not text or text.lower() == "quit":
                print("[Goodbye]")
                break
            await self.run_once(text_input=text)

    async def close(self) -> None:
        """Release all held resources.

        Why: Prevents resource-leak warnings at process exit.
        What: Closes daemon HTTP client, TTS speaker, and mic recorder.
        Test: Call close(); assert no exception.
        """
        await self._daemon.aclose()
        if hasattr(self._speaker, "close"):
            self._speaker.close()  # type: ignore[union-attr]
        if self._recorder is not None:
            self._recorder.close()
