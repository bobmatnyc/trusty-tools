"""
CLI entry point for trusty-voice.

Why: Keeps the main function thin — parse args, load env, build config, run loop.
What: Loads .env.local from the repo root (or CWD), validates credentials,
     instantiates the pipeline, and runs either text or audio mode.
Test: Run ``python -m trusty_voice --help``; assert exit code 0 and usage text.
     Run with --text and stdin piped to a fixture; assert [TTS] output appears.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys
from pathlib import Path


def _find_env_file() -> Path | None:
    """Locate .env.local, searching from CWD up to 4 levels.

    Why: The canonical key file lives in the repo root, but the user may run
         trusty-voice from within a subdirectory.
    What: Walks up the directory tree looking for .env.local.
    Test: Create a temp .env.local; change CWD; assert _find_env_file() returns
          its path.
    """
    here = Path.cwd()
    for candidate in [here, *here.parents[:4]]:
        f = candidate / ".env.local"
        if f.exists():
            return f
    return None


def _load_env() -> None:
    """Load credentials from .env.local without printing key values.

    Why: python-dotenv is the canonical way to load .env files; we call it
         early before argparse so env vars are available to VoiceConfig.
    What: Finds .env.local; loads it via dotenv; logs the path (not the values).
    Test: Write a temp .env.local with DEEPGRAM_API_KEY=test; call _load_env();
          assert os.environ["DEEPGRAM_API_KEY"] == "test".  # pragma: allowlist secret
    """
    try:
        from dotenv import load_dotenv  # type: ignore[import]
    except ImportError:
        # python-dotenv is a hard dependency; a missing import means the venv
        # is broken.  Surface a warning so the user gets a useful hint instead
        # of a cryptic "missing API key" error later.
        import sys

        print(
            "[trusty-voice] WARNING: python-dotenv not found — .env.local will not be loaded. "
            "Run 'uv sync' to repair the environment.",
            file=sys.stderr,
        )
        return

    env_path = _find_env_file()
    if env_path:
        load_dotenv(env_path, override=False)
        logging.getLogger(__name__).debug("Loaded env from %s", env_path)
    else:
        logging.getLogger(__name__).debug(
            ".env.local not found — relying on existing environment variables"
        )


def _build_arg_parser() -> argparse.ArgumentParser:
    """Build the CLI argument parser.

    Why: Centralises arg definitions; tested separately from the async entry.
    What: Defines --text / -t flag and --daemon-url / --voice-id overrides.
    Test: Parse ["--text"]; assert namespace.text is True.
    """
    p = argparse.ArgumentParser(
        prog="trusty-voice",
        description=(
            "Phase 0 push-to-talk voice interface for trusty-mpm agents. "
            "Requires DEEPGRAM_API_KEY and ELEVENLABS_API_KEY in .env.local."
        ),
    )
    p.add_argument(
        "--text",
        "-t",
        action="store_true",
        help="Text (dry-run) mode: type messages instead of speaking. No audio hardware needed.",
    )
    p.add_argument(
        "--daemon-url",
        default=None,
        help="Override the trusty-mpm daemon URL (default: http://127.0.0.1:7880)",
    )
    p.add_argument(
        "--voice-id",
        default=None,
        help="ElevenLabs voice ID to use for TTS (overrides TRUSTY_VOICE_TTS_VOICE_ID)",
    )
    p.add_argument(
        "--log-level",
        default="WARNING",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Log verbosity (default: WARNING)",
    )
    return p


async def _async_main(args: argparse.Namespace) -> int:
    """Async entry point.

    Why: Keeps asyncio.run() in main() so the async body is testable directly.
    What: Builds VoiceConfig, creates VoicePipeline, runs the appropriate loop.
    Test: Monkeypatch VoiceConfig.from_env(); call _async_main(args); assert
          pipeline.run_text_loop() was called in text mode.
    """
    from trusty_voice.config import VoiceConfig
    from trusty_voice.pipeline import VoicePipeline

    # Apply CLI overrides to environment before building config
    if args.daemon_url:
        os.environ["TRUSTY_VOICE_DAEMON_URL"] = args.daemon_url
    if args.voice_id:
        os.environ["TRUSTY_VOICE_TTS_VOICE_ID"] = args.voice_id

    try:
        config = VoiceConfig.from_env()
    except ValueError as exc:
        print(f"[trusty-voice] Configuration error: {exc}", file=sys.stderr)
        print(
            "\nHint: copy .env.local from the repo root and set your API keys:\n"
            "  DEEPGRAM_API_KEY=...\n"
            "  ELEVENLABS_API_KEY=...",
            file=sys.stderr,
        )
        return 1

    # Apply text_mode from CLI flag
    import dataclasses

    config = dataclasses.replace(config, text_mode=args.text)

    logger = logging.getLogger(__name__)
    logger.info("Starting trusty-voice with config: %s", config.redacted())

    pipeline = VoicePipeline(config=config)
    try:
        if config.text_mode:
            await pipeline.run_text_loop()
        else:
            await pipeline.run_loop()
    finally:
        await pipeline.close()

    return 0


def main() -> None:
    """Synchronous entry point called by the trusty-voice script.

    Why: uv / pip install creates a script entry that expects a plain callable.
    What: Parses args, loads env, runs the async main, exits with the return code.
    Test: subprocess.run(["trusty-voice", "--help"]); assert returncode == 0.
    """
    _load_env()

    parser = _build_arg_parser()
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level),
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
        stream=sys.stderr,
    )

    sys.exit(asyncio.run(_async_main(args)))


if __name__ == "__main__":
    main()
