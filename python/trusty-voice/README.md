# trusty-voice — Phase 0 push-to-talk prototype

Voice interface for the trusty-mpm coding agent.
Pipeline: Mic (push-to-talk) → Deepgram STT → trusty-mpm daemon → ElevenLabs TTS → Speaker.

Built with [Pipecat](https://www.pipecat.ai/), managed with [uv](https://docs.astral.sh/uv/).

## Phase 0 scope

- Push-to-talk (press Enter → speak → release Enter → agent replies aloud)
- Deepgram nova-2 for STT (cloud, pre-recorded in Phase 0)
- trusty-mpm local daemon on `http://127.0.0.1:7880` for agent responses
- ElevenLabs Turbo v2 for TTS
- `--text` / dry-run mode: type instead of speaking, print instead of playing

## Prerequisites

### 1. System dependencies

```bash
# macOS (required for pyaudio mic/speaker access)
brew install portaudio
```

### 2. Python tooling

```bash
# Install uv if you don't have it
curl -LsSf https://astral.sh/uv/install.sh | sh
```

### 3. API keys

The keys are loaded from `.env.local` in the repo root (already gitignored).
Add these two lines:

```bash
# in /path/to/trusty-tools/.env.local
DEEPGRAM_API_KEY=your_deepgram_api_key_here
ELEVENLABS_API_KEY=your_elevenlabs_api_key_here
```

You can also override the daemon URL and other settings:

```bash
TRUSTY_VOICE_DAEMON_URL=http://127.0.0.1:7880   # default
TRUSTY_VOICE_STT_LANGUAGE=en-US                  # default
TRUSTY_VOICE_STT_MODEL=nova-2                    # default
TRUSTY_VOICE_TTS_VOICE_ID=DODLEQrClDo8wCz460ld  # ElevenLabs "Lauren B" (default; River SAz9YHcvj6GT2YYXdXww and Rachel 21m00Tcm4TlvDq8ikWAM are retired)
TRUSTY_VOICE_TTS_MODEL_ID=eleven_turbo_v2        # default
TRUSTY_VOICE_CONV_ID=                            # leave blank for a fresh session
```

### 4. Microphone permission (macOS TCC — MANUAL STEP)

**You must grant microphone access manually. This cannot be scripted.**

1. Run the app once: `uv run trusty-voice` (it will fail/hang without the grant).
2. macOS will show a "Terminal wants to access the microphone" dialog — click **Allow**.
3. Alternatively, go to **System Settings → Privacy & Security → Microphone** and
   toggle on your terminal app (Terminal.app, iTerm2, Warp, etc.).

After granting access, re-run the app — it will record normally.

## Installation

Run from this directory (`python/trusty-voice/`):

```bash
uv sync
```

This installs all dependencies (including dev tools) into `.venv/`.

## Running

### Push-to-talk mode (full audio)

Requires: mic grant, portaudio, DEEPGRAM_API_KEY, ELEVENLABS_API_KEY, and
the trusty-mpm daemon running on port 7880.

```bash
# from python/trusty-voice/
uv run trusty-voice
```

Press **Enter** to start recording, speak, press **Enter** again to stop.
The agent reply is spoken aloud. Press **Ctrl-C** to quit.

### Text (dry-run) mode

No audio hardware, no live API keys needed for the pipeline loop itself
(though Deepgram/ElevenLabs are still imported — use `--text` to bypass them).
The daemon still needs to be running for real responses; if it's not, the
loop gracefully prints an error and continues.

```bash
uv run trusty-voice --text
```

Type a message and press Enter. The agent reply is printed as `[TTS] <reply>`.
Type `quit` or press **Ctrl-C** to exit.

### Options

```
--text, -t           Text (dry-run) mode — no audio hardware needed
--daemon-url URL     Override daemon base URL (default: http://127.0.0.1:7880)
--voice-id ID        ElevenLabs voice ID override
--log-level LEVEL    Logging verbosity: DEBUG | INFO | WARNING | ERROR (default: WARNING)
```

### Module form

```bash
uv run python -m trusty_voice --help
uv run python -m trusty_voice --text
```

## Testing

```bash
# All tests (no audio hardware or API keys needed)
uv run pytest

# With coverage
uv run pytest --cov=src/trusty_voice --cov-report=term-missing

# Specific test file
uv run pytest tests/test_daemon_client.py -v
```

## Linting / formatting

```bash
uv run ruff check src/ tests/
uv run ruff format --check src/ tests/
uv run ruff format src/ tests/   # auto-fix
```

## Architecture

```
trusty_voice/
├── __init__.py       — package root, public re-exports
├── __main__.py       — CLI entry point (argparse + asyncio.run)
├── config.py         — VoiceConfig (frozen dataclass, from_env())
├── daemon_client.py  — DaemonClient (httpx → POST /api/v1/sessions/chat)
├── stt.py            — DeepgramTranscriber + TextPassthroughTranscriber
├── tts.py            — ElevenLabsSpeaker + TextOnlySpeaker
├── audio.py          — MicRecorder (push-to-talk via pyaudio)
└── pipeline.py       — VoicePipeline (orchestrates one utterance cycle)
```

**Data flow (audio mode):**

```
Enter pressed
  → MicRecorder.record_blocking()     [pyaudio → WAV bytes]
  → DeepgramTranscriber.transcribe_bytes()  [WAV → transcript str]
  → DaemonClient.send_message()       [transcript → POST :7880 → reply str]
  → ElevenLabsSpeaker.speak()         [reply → audio → pyaudio output]
Enter pressed again to start next cycle
```

**Data flow (text mode `--text`):**

```
[You] > <typed text>
  → VoicePipeline.run_once(text_input=<typed text>)
  → DaemonClient.send_message()       [text → POST :7880 → reply str]
  → TextOnlySpeaker.speak()           [reply → print "[TTS] <reply>"]
```

## Daemon requirement

The trusty-mpm daemon must be running locally for real agent responses:

```bash
# In another terminal (from the repo root):
cargo run -p trusty-mpm -- start
# or, if already installed:
trusty-mpm start
```

If the daemon is not running, `DaemonClient` raises `DaemonError` and the
pipeline prints `[Error: Failed to reach daemon ...]` — it does NOT crash.

## Phase roadmap (from epic #1561)

| Phase | Feature |
|-------|---------|
| 0 (this) | Push-to-talk: Pipecat + Deepgram + ElevenLabs + `:7880/api/v1/sessions/chat` |
| 1 | Wake word ("Hey Trusty") + speaker verification (Picovoice) |
| 2 | Streaming SSE `coordinator_chat` variant (speak while generating) |
| 3 | Token-auth gateway + Linux thin web client (WebRTC kiosk) |

## License

MIT — same as the parent trusty-tools workspace.
