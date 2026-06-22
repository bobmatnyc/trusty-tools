# DOC-25 — trusty-voice — Streaming Voice Interface to the Coding Agent

**Status:** Draft
**Subsystem:** trusty-voice (new) — voice client over the trusty-mpm chat surface
**Owner:** Engineering (trusty-voice)
**Last-updated:** 2026-06-22
**Spec ID:** `SPEC-VOICE-01~draft` … `-08~draft` (DOC-25)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`,
the `SessionRecord` / `ManagedSessionId` / lifecycle contract that trusty-voice sessions
consume but do not re-specify); DOC-24 — Standalone Managed `trusty-mpm` Driver
(`docs/specs/standalone-managed-trusty-mpm.md`, the managed session and `coordinator_chat`
surface that trusty-voice calls as its backend).
**Cross-ref:** `crates/trusty-mpm/src/daemon/api.rs` (coordinator chat, session events, per-session
events — cited in SPEC-VOICE-02); `crates/trusty-mpm/src/daemon/api/coordinator_routes.rs`
(COORDINATOR_CHAT_PATH, CSRF guard — SPEC-VOICE-02); `crates/trusty-mpm/src/core/discovery.rs`
(DEFAULT_DAEMON_ADDR — SPEC-VOICE-02); `crates/trusty-common/src/server.rs` (wide-open CORS
note — SPEC-VOICE-07).

> **Scope note.** This is a **behavior contract** for **trusty-voice**, a new subsystem that
> does not yet exist as a crate in this workspace. The spec is design-stage: no implementation
> has landed; the spec governs what the implementation must do. trusty-voice is a streaming
> voice front-end — a client — that drives the trusty-mpm coding agent via the existing
> `coordinator_chat` HTTP surface. It does **not** re-specify the session lifecycle (DOC-14),
> the `coordinator_chat` routing internals, the provisioner, or the autonomy tiers (DOC-23).
> It specifies only the voice client layer: the pipeline contract, the component stack, speaker
> verification, latency, deployment targets, remote security, and the phased roadmap.

> **Pricing/latency note.** Vendor pricing and latency figures cited in this spec (Deepgram,
> ElevenLabs, Picovoice) are **indicative only** and were collected during a web-research pass
> that was rate-limited. All figures should be re-verified against current vendor pricing pages
> before committing to a budget or SLA.

---

## Purpose & Scope

trusty-voice is a streaming voice interface that lets an engineer speak to the trusty-mpm
coding agent and hear its replies — without touching a keyboard. Its v1 north-star is
"talk to any trusty agent"; its v1 scope is narrower: attended push-to-talk and wake-word
interaction with the trusty-mpm coordinator. Voice is a thin client layer: it converts audio
to text, sends that text to the coordinator, converts the text reply to audio, and plays it
back. All agent reasoning and session management remain in trusty-mpm exactly as today.

**Out of scope (consumed, not re-specified):** the session lifecycle (DOC-14), the
`coordinator_chat` routing internals and session-manager agent, the provisioner, the
autonomy tiers (DOC-23). trusty-voice is a **client** over these existing surfaces.

## Table of Contents

| ID | Section |
|----|---------|
| SPEC-VOICE-01~draft | [Purpose, north-star & two-layer scope](#1-purpose-north-star--two-layer-scope-spec-voice-01draft) |
| SPEC-VOICE-02~draft | [v1 Conversational Binding to trusty-mpm `coordinator_chat`](#2-v1-conversational-binding-to-trusty-mpm-coordinator_chat-spec-voice-02draft) |
| SPEC-VOICE-03~draft | [Voice Pipeline & Component Stack](#3-voice-pipeline--component-stack-spec-voice-03draft) |
| SPEC-VOICE-04~draft | [Personalized Voice Detection (Speaker Verification) Contract](#4-personalized-voice-detection-speaker-verification-contract-spec-voice-04draft) |
| SPEC-VOICE-05~draft | [Streaming & Latency Contract](#5-streaming--latency-contract-spec-voice-05draft) |
| SPEC-VOICE-06~draft | [Deployment Targets](#6-deployment-targets-spec-voice-06draft) |
| SPEC-VOICE-07~draft | [Remote-Access & Security Contract](#7-remote-access--security-contract-spec-voice-07draft) |
| SPEC-VOICE-08~draft | [Phased Roadmap & "Runs on This Mac Today" Prototype](#8-phased-roadmap--runs-on-this-mac-today-prototype-spec-voice-08draft) |

---

## 1. Purpose, north-star & two-layer scope {#SPEC-VOICE-01~draft}

**ID:** SPEC-VOICE-01~draft
**Status:** Draft

### Behavior Contract (WHAT)

trusty-voice is a **streaming voice front-end** whose v1 target is the trusty-mpm coding
agent. It is not an autonomous agent, a session manager, or a reasoning layer.

**North-star (long-range goal):** "Talk to any trusty agent." A user speaks; the appropriate
trusty agent — coding, memory, search, or a future specialist — replies in speech. The routing
between agents is handled by the trusty-mpm coordinator; trusty-voice does not need to know
which downstream agent answers.

**v1 scope (two-layer definition):**

- **Layer 1 — attended voice input (v1).** The user initiates a turn by pressing a push-to-talk
  key or by speaking the configured wake word ("Hey Trusty"). Voice-activity detection determines
  when the utterance ends. The captured audio is transcribed and sent to `coordinator_chat`.
  The reply is synthesized to audio and played back. The user is present and in control at all
  times. trusty-voice does **not** drive autonomous agent sessions, issue commands without
  human confirmation, or invoke agent tools on the user's behalf.

- **Layer 2 — autonomous voice (future, out of scope for v1).** An imagined future where
  trusty-voice acts as the speech surface for a semi-autonomous session (DOC-23 autonomy tiers).
  This is a north-star direction, not a v1 deliverable, and is not governed by this spec.

**Inputs (v1):** audio captured from the local microphone; push-to-talk or wake-word trigger;
optional `conv_id` from a prior turn for multi-turn continuity.

**Outputs (v1):** audio played to the local speaker; agent reply text (for display); updated
`conv_id` for the next turn.

**Preconditions:** trusty-mpm daemon running and listening on `127.0.0.1:7880` (or the
configured bind address); at least one managed session active; speaker enrolled (Phase 1+) or
speaker verification disabled (Phase 0).

**Postconditions:** the user's spoken intent has been delivered to the agent, the agent's
text reply has been synthesized and played, and the conversation state (`conv_id`) has been
updated for the next turn.

**Error behavior:** if the daemon is unreachable, trusty-voice plays or displays a
"not connected" signal and does not attempt to transcribe or synthesize. If STT fails or
returns an empty transcript, the turn is discarded silently with a brief audio cue. If TTS
fails, the text reply is displayed but no audio plays.

### Rationale (WHY)

Keeping v1 strictly attended and narrowly scoped to the trusty-mpm coordinator reduces the
surface area of the first implementation to a single HTTP call per turn. The north-star
("any trusty agent") is stated explicitly so that future extensibility — to trusty-memory
queries, trusty-search queries, or multi-agent routing — is designed in from the start without
requiring v1 to implement it.

---

## 2. v1 Conversational Binding to trusty-mpm `coordinator_chat` {#SPEC-VOICE-02~draft}

**ID:** SPEC-VOICE-02~draft
**Status:** Draft

### Behavior Contract (WHAT)

trusty-voice calls the existing trusty-mpm daemon directly. No new agent server code is
required on localhost.

**Primary chat call:**

```
POST http://127.0.0.1:7880/api/v1/sessions/chat
Content-Type: application/json

{"message": "<transcript>", "conv_id": "<prior conv_id or omit for new>"}
```

Response:

```json
{"reply": "<agent text>", "conv_id": "<updated>", "routed_to_session": "<id>", "command_output": null}
```

- `COORDINATOR_CHAT_PATH = "/api/v1/sessions/chat"` is declared as a constant in
  `crates/trusty-mpm/src/daemon/api/coordinator_routes.rs:33`.
- The daemon bind address defaults to `DEFAULT_DAEMON_ADDR = "127.0.0.1:7880"` declared in
  `crates/trusty-mpm/src/core/discovery.rs:27`.
- Route implementation lives in `crates/trusty-mpm/src/daemon/api.rs` approximately lines 125–248.

**Multi-turn continuity:** the `conv_id` field returned by each reply must be stored by the
voice client and echoed on the next call. Omitting `conv_id` starts a new conversation. The
daemon manages conversation state; the voice client manages the `conv_id` token only.

**CSRF origin guard:** the coordinator routes apply an origin guard
(`crates/trusty-mpm/src/daemon/api/coordinator_routes.rs:193–196`). Server-side callers
(including trusty-voice running on the same host as the daemon) do **not** send an `Origin`
header and therefore pass the guard without any additional configuration.

**Agent interrupt / pending-decision flow:**

When the agent pauses and asks the user a question — a "pending decision" — the voice client
must surface the question in audio and accept a spoken answer.

1. Poll or stream `GET /sessions/{id}/events` (SSE; route implementation in `daemon/api.rs`
   approximately lines 346–361) for `pending_decision` events carrying a `proposed_default`.
2. Announce the pending decision in TTS ("The agent is asking: <question>. Default: <default>.
   Say 'yes', 'no', or your answer.").
3. Accept a short push-to-talk utterance; transcribe it.
4. Submit via `POST /sessions/{id}/answer {"answer": "<transcript>"}`.

The per-session activity endpoint (`daemon/api.rs` approximately lines 771–795) also surfaces
`pending_decision` and `proposed_default` fields and may be polled as an alternative to SSE.

**Inputs:** transcript string (from STT); optional `conv_id`.
**Outputs:** `reply` string (to TTS); updated `conv_id`; optional `routed_to_session`,
`command_output` (for display).
**Preconditions:** daemon reachable on the configured address; at least one managed session active.
**Postconditions:** the transcript has been delivered to the coordinator; a reply string has
been received; `conv_id` has been updated.
**Error conditions:** HTTP 4xx/5xx → surface error text in TTS or UI, retain prior `conv_id`;
network timeout (suggest 30 s) → play "no response" cue, do not discard `conv_id`.

### Rationale (WHY)

Binding directly to the existing `coordinator_chat` HTTP endpoint means trusty-voice adds
zero lines to the Rust daemon in v1. The endpoint is already multi-turn-capable (via
`conv_id`), already routes to the correct session, and already handles pending decisions via
the events/answer mechanism. The voice client is a thin wrapper around these existing
primitives.

---

## 3. Voice Pipeline & Component Stack {#SPEC-VOICE-03~draft}

**ID:** SPEC-VOICE-03~draft
**Status:** Draft

### Behavior Contract (WHAT)

The v1 voice pipeline is a sequential loop over the following stages. Each stage is
independently swappable; the component choices below are **recommendations for v1**, not
contractual requirements. The contract is the stage interface (input/output types),
not the vendor.

```
[wake word / PTT trigger]
        ↓
[speaker verification]        ← (Phase 1+; pass-through in Phase 0)
        ↓
[VAD / endpointing]           ← determines utterance boundary
        ↓
[streaming STT]               ← returns transcript
        ↓
[coordinator_chat HTTP call]  ← returns agent text reply
        ↓
[streaming TTS]               ← returns audio stream
        ↓
[playback]                    ← with barge-in cancellation
```

**Recommended v1 component stack:**

| Stage | Recommended component | Notes |
|-------|----------------------|-------|
| Orchestration | **Pipecat** (Python, open-source) | Built-in Deepgram/ElevenLabs/Silero services; interruption support; `coordinator_chat` wrapped as a custom processor (the "LLM" step in pipeline terms). |
| STT | **Deepgram Nova-3** (streaming WebSocket) | Interim transcripts + endpointing. Indicative pricing: ~$0.0043/min (re-verify). |
| TTS | **ElevenLabs Flash v2.5** (streaming WebSocket) | Low time-to-first-byte for perceived responsiveness. Indicative first-audio latency: 75–150 ms (re-verify). |
| VAD | **Silero** (via Pipecat's built-in service) | Runs locally; no external API. |
| Wake word | **Picovoice Porcupine** with custom "Hey Trusty" keyword | Shares one `PICOVOICE_ACCESS_KEY` with Eagle. |
| Speaker verification | **Picovoice Eagle** | Runs locally against enrolled voice profile. |
| Playback | macOS `afplay` / Pipecat local audio transport | Barge-in: cancel in-flight TTS on new wake/PTT trigger. |

**Barge-in contract:** when a new wake-word or PTT event fires while TTS is playing, the
current TTS audio stream must be cancelled and playback stopped before the new turn begins.
This is a required behavior, not optional.

**Pipecat integration point:** the `coordinator_chat` call is implemented as a Pipecat
`LLMService`-equivalent custom processor. It receives the final transcript from the STT
service, posts to `http://127.0.0.1:7880/api/v1/sessions/chat`, and emits the reply string
downstream to the TTS service.

**Inputs to pipeline:** raw PCM audio from the microphone.
**Outputs from pipeline:** PCM audio to the speaker; text transcript and reply for optional display.
**Preconditions:** microphone access granted (macOS TCC for the terminal process); speakers
available; Pipecat environment configured with `DEEPGRAM_API_KEY`, `ELEVENLABS_API_KEY`
(Phase 0); additionally `PICOVOICE_ACCESS_KEY` for Phase 1+.
**Postconditions:** each completed turn has been logged (transcript + reply) for debugging;
the pipeline is ready for the next turn.
**Error conditions:** STT WebSocket drop → reconnect with exponential backoff; TTS error →
display text, skip audio; Pipecat pipeline crash → restart the process (the daemon is
unaffected).

### Rationale (WHY)

Pipecat is chosen for v1 because the Python voice ecosystem (Deepgram, ElevenLabs, Picovoice
SDKs) is mature and well-maintained; Pipecat wraps these into a composable pipeline with
built-in interruption/barge-in support. The `coordinator_chat` call is a single HTTP POST —
a trivial custom processor — so the full pipeline can be built without any Rust code in Phase 0.
Component recommendations are stated as recommendations to preserve the freedom to swap vendors
without re-speccing the pipeline contract.

---

## 4. Personalized Voice Detection (Speaker Verification) Contract {#SPEC-VOICE-04~draft}

**ID:** SPEC-VOICE-04~draft
**Status:** Draft

### Behavior Contract (WHAT)

Speaker verification ensures that only the enrolled owner's voice reaches the coding agent.
Utterances from an unrecognized voice are silently discarded before transcription.

**Enrollment (one-time):**

- The user runs `trusty-voice enroll` (or equivalent setup command).
- The system records approximately 20 seconds of speech from the user.
- Picovoice Eagle processes the recording and writes an enrollment profile to a local file
  (e.g. `~/.trusty-voice/speaker-profile.pv`).
- Enrollment is idempotent: re-running replaces the profile.
- **No audio is sent to a remote server during enrollment.** Eagle runs fully locally.

**Verification (per utterance):**

- After VAD endpointing captures a complete utterance, Eagle scores it against the enrolled
  profile, returning a similarity score in [0, 1].
- **Accept threshold:** score ≥ configured threshold (default 0.5; tunable). The accepted
  utterance proceeds to STT.
- **Reject behavior:** score below threshold → the utterance is discarded; a brief inaudible
  (or optionally a short audio cue) signals the rejection. The pipeline returns to the listening
  state without sending anything to STT or the agent.
- **False-accept concern:** a low threshold risks accepting voices other than the enrolled
  speaker. The default (0.5) is a conservative starting point; users operating in shared
  environments should raise the threshold. This is a configuration responsibility, not an
  automatic system behavior.

**Preconditions:** enrollment profile exists at the configured path; `PICOVOICE_ACCESS_KEY`
in environment.
**Postconditions (accept):** the utterance has been verified as the enrolled speaker and is
forwarded to STT unchanged.
**Postconditions (reject):** no audio, transcript, or message has been forwarded to STT,
`coordinator_chat`, or any external service.
**Error conditions:** missing enrollment profile → fail with a diagnostic advising enrollment;
Eagle initialization failure (bad access key, model file missing) → fail closed (do not allow
unverified audio through).

**v1 scope:** single enrolled speaker. Multi-user enrollment (multiple profiles, per-user
routing) is a future extension and is not governed by this spec.

### Rationale (WHY)

Speaker verification prevents a shared-office or co-located scenario from sending a colleague's
voice to the user's coding agent. Running Eagle locally (no remote API) means enrollment
audio never leaves the machine. Fail-closed on enrollment error is mandatory: silently accepting
unverified utterances when the profile is missing would make the feature invisible and misleading.

---

## 5. Streaming & Latency Contract {#SPEC-VOICE-05~draft}

**ID:** SPEC-VOICE-05~draft
**Status:** Draft

### Behavior Contract (WHAT)

**STT is streaming.** Deepgram Nova-3 delivers interim transcript results over a WebSocket
while the user is still speaking. These interim results are used for display only (a live
transcript) in v1; the final transcript is sent to `coordinator_chat`.

**Barge-in cancels in-flight TTS.** This is a hard contract: any new wake-word or PTT
event during TTS playback must immediately cancel the current audio stream and stop playback.
The pipeline must not buffer pending TTS chunks after cancellation.

**IMPORTANT CAVEAT — `coordinator_chat` is not streaming today:**
`POST /api/v1/sessions/chat` returns a **single JSON reply** (`{"reply": "…"}`). It is not
a streaming/SSE endpoint. This means in v1, TTS synthesis does **not begin until the full
agent reply has been received** from the coordinator. There is an inherent latency floor
equal to the agent's end-to-end processing time.

**True speak-while-generating** (TTS begins as the first tokens of the agent reply arrive)
**requires a streaming-SSE variant of `coordinator_chat`** that emits reply chunks incrementally.
This is a scoped follow-up (Phase 2 in §SPEC-VOICE-08~draft). A reference pattern exists:
the trusty-memory daemon implements a streaming chat handler (approximately `lib.rs:1925`)
that can be adapted for the coordinator.

**Indicative end-to-end latency budget (v1, per turn):**

| Stage | Indicative latency |
|-------|--------------------|
| Wake word detection | < 50 ms (local, continuous) |
| VAD endpointing (after utterance ends) | ~100–300 ms |
| STT first partial result | ~300 ms after speech start |
| STT final transcript | ~200–500 ms after utterance end |
| `coordinator_chat` agent reply | variable; depends on agent complexity (100 ms – 30 s) |
| ElevenLabs Flash v2.5 first audio chunk | ~75–150 ms after reply received |
| Playback start | < 50 ms after first chunk |

**All figures are indicative and pending re-verification.** The dominant variable is agent
processing time, which this spec does not bound (it is governed by the agent and the task).

**Preconditions:** STT WebSocket connected; TTS WebSocket ready; pipeline in listening state.
**Postconditions per turn:** transcript sent; reply received; audio played to completion or
cancelled by barge-in; pipeline returns to listening state.
**Error conditions (latency):** `coordinator_chat` timeout (suggested 30 s) → play "no response"
audio cue; TTS WebSocket timeout → display text, skip audio; STT dropout mid-utterance →
discard partial transcript, play re-prompt cue.

### Rationale (WHY)

Stating the single-reply limitation explicitly prevents v1 implementers from assuming
streaming agent output is available and building a pipeline that silently blocks awaiting
chunks that never arrive. The Phase 2 streaming extension is called out here so that the
`coordinator_chat` route owner knows to design the streaming SSE variant with the voice
client in mind.

---

## 6. Deployment Targets {#SPEC-VOICE-06~draft}

**ID:** SPEC-VOICE-06~draft
**Status:** Draft

### Behavior Contract (WHAT)

trusty-voice v1 supports two deployment configurations.

#### (a) Mac mini all-in-one (primary v1 target)

The Pipecat application runs the complete voice pipeline — wake word, speaker verification,
VAD, STT, coordinator call, TTS, playback — on the same Mac mini that runs the trusty-mpm
daemon.

- Microphone and speakers are local to the Mac mini (or a connected USB audio device).
- The `coordinator_chat` call is a localhost HTTP POST to `127.0.0.1:7880`.
- No remote access, no authentication gateway required beyond the local macOS TCC mic grant.
- Startup: `uv run python trusty_voice/main.py` (or equivalent) in the Python venv.

#### (b) Linux small-form-factor thin web client (Phase 3)

The trusty HOST (Mac mini or Linux server) runs the complete Pipecat pipeline (STT, agent
call, TTS) plus the trusty-mpm daemon. A lightweight Linux small-form-factor (SFF) device
acts as a dumb audio terminal:

**SFF device characteristics:**
- Hardware: Raspberry Pi 5 or an N100 mini-PC (or equivalent low-power x86/ARM board).
- OS: lightweight distribution — DietPi, Raspberry Pi OS Lite, or similar. No desktop
  environment required.
- Browser: Chromium in kiosk mode (autostart, full-screen, minimal flags).
- Role: capture microphone audio via `getUserMedia` in the browser; stream it to the HOST
  over **WebRTC** (Pipecat WebRTC transport); play back the TTS audio stream received over
  the same WebRTC connection. The SFF performs no STT, no TTS synthesis, no agent calls.

The SFF is **truly thin**: all intelligence lives on the HOST. The SFF only handles raw
audio I/O and WebRTC transport. This makes the SFF disposable and cheap to replace.

**Inputs (config per target):**
- (a) Local audio device path; `ELEVENLABS_API_KEY`, `DEEPGRAM_API_KEY` in environment.
- (b) Additionally: WebRTC signaling endpoint; token-auth gateway address (§SPEC-VOICE-07~draft).

**Preconditions per target:**
- (a) Mac TCC microphone grant for the terminal process; Pipecat venv activated.
- (b) HOST Pipecat pipeline exposing a WebRTC endpoint; SFF network reachable to HOST.

**Postconditions:** the voice pipeline is operational; a user speaking into the
microphone (local for (a), via browser `getUserMedia` for (b)) receives agent replies
as audio.

**Error conditions:** (a) daemon unreachable → "not connected" audio cue; (b) WebRTC
negotiation failure → display error in kiosk browser; HOST pipeline down → kiosk shows
reconnect indicator.

### Rationale (WHY)

The Mac mini all-in-one target is the simplest possible deployment: no networking, no auth,
no latency over WebRTC. It is the right Phase 0–2 target. The SFF WebRTC target is included
because it is the natural extension for a dedicated always-on voice terminal in a workspace —
the SFF stays plugged in at the desk while the heavy processing remains on the development
machine.

---

## 7. Remote-Access & Security Contract {#SPEC-VOICE-07~draft}

**ID:** SPEC-VOICE-07~draft
**Status:** Draft

### Behavior Contract (WHAT)

**Current daemon security posture (verified):**

- trusty-mpm, trusty-memory, and trusty-search bind to `127.0.0.1` with **no authentication**.
- trusty-agents binds to `0.0.0.0` (all interfaces).
- trusty-search and trusty-memory use wide-open CORS (`allow_origin(Any)` in
  `crates/trusty-common/src/server.rs:50`).

For the **Mac mini all-in-one deployment** (§SPEC-VOICE-06~draft (a)), this is acceptable:
the voice client is a process on the same host; `127.0.0.1` is the binding; no remote
access is required. No additional auth is needed for this deployment target.

For **any remote access scenario** — including the SFF WebRTC thin-client target
(§SPEC-VOICE-06~draft (b)) and any scenario where the Pipecat pipeline runs on a
different host from the daemon — the following contract applies:

**Token-auth gateway (REQUIRED for remote):**

A token-authentication gateway must sit in front of the voice-relevant trusty-mpm endpoints
before any remote-access deployment is permitted:

- `POST /api/v1/sessions/chat`
- `GET /sessions/managed/{id}/activity`
- `POST /sessions/managed/{id}/answer`

The gateway enforces a bearer token on every inbound request. Requests without a valid
token are rejected with HTTP 401.

**Reference implementation patterns (existing in-tree code):**

- `crates/trusty-mpm/src/daemon/mod.rs:242–258` — `spawn_secondary_listener` hook: a pattern
  for launching a secondary HTTP listener (e.g. on a different port or interface) that can
  host the authenticated gateway surface without mutating the primary localhost listener.
- `crates/trusty-agents/src/api/server/auth.rs` — optional bearer-token middleware already
  implemented for the trusty-agents HTTP server; adaptable to trusty-mpm.

**Tailscale as interim shortcut:** for personal-use or development deployments, running the
HOST and the SFF on the same Tailscale network provides WireGuard-encrypted private
connectivity without implementing the gateway. This is an acceptable interim shortcut but
does not substitute for the token-auth gateway in a multi-user or production context.

**Preconditions (remote deployment):**
- Token-auth gateway deployed and configured with a shared secret between the HOST pipeline
  and the remote voice client.
- TLS in place on the gateway endpoint (either via Tailscale encryption or a terminating
  reverse proxy).

**Postconditions (remote deployment):** only requests bearing a valid bearer token reach the
coordinator endpoints; the daemon's `127.0.0.1` binding is not exposed to the remote network.

**Error conditions:** missing or invalid token → HTTP 401, voice client plays "not authorized"
cue; gateway unreachable → voice client plays "not connected" cue.

### Rationale (WHY)

trusty-mpm's no-auth localhost posture is appropriate for a single-user developer machine.
Extending it to a remote voice client without auth would expose the coding agent's
session-control surface — including the ability to send arbitrary messages to active coding
sessions — to any network-adjacent process. The token-auth gateway contract is the minimum
security bar for remote access. The existing `spawn_secondary_listener` and bearer-token
middleware patterns mean this gate can be implemented without architectural changes to the
daemon core.

---

## 8. Phased Roadmap & "Runs on This Mac Today" Prototype {#SPEC-VOICE-08~draft}

**ID:** SPEC-VOICE-08~draft
**Status:** Draft

### Behavior Contract (WHAT)

The roadmap is divided into four phases. Each phase is independently shippable and builds
on the previous.

#### Phase 0 — Push-to-talk core (runs on this Mac today)

**Goal:** a minimal voice loop that demonstrates the concept with no Picovoice dependency.

**Components:** Pipecat + Deepgram Nova-3 (STT) + ElevenLabs Flash v2.5 (TTS) → POST to
`127.0.0.1:7880/api/v1/sessions/chat`.

**Prerequisites:**

```bash
brew install portaudio          # system audio I/O for Pipecat
uv venv .venv && source .venv/bin/activate
uv pip install pipecat-ai deepgram-sdk elevenlabs
```

Environment (`.env.local`, already present per project conventions):

```
ELEVENLABS_API_KEY=<key>
DEEPGRAM_API_KEY=<key>
```

macOS TCC: grant microphone access to Terminal (or whichever terminal runs the Python
process) in **System Settings → Privacy & Security → Microphone**.

No Picovoice access key is needed. Activation is push-to-talk (PTT) only: the user holds
a key while speaking; the recording stops on key release; the transcript is sent immediately.

**Deliverable:** a Python script (`trusty_voice/main.py` in the (new) `trusty-voice`
directory, or a standalone script) that runs the Phase 0 loop. No crate, no Cargo.

#### Phase 1 — Wake word + speaker verification

**Goal:** hands-free "Hey Trusty" activation with personal-voice gating.

**New components:** Picovoice Porcupine (wake word with custom "Hey Trusty" keyword model)
+ Picovoice Eagle (speaker verification, §SPEC-VOICE-04~draft).

**Prerequisites:** `PICOVOICE_ACCESS_KEY` in `.env.local`; `uv pip install pvporcupine pvrecorder pveagle`.

**Deliverable:** enrollment command (`trusty-voice enroll`) writes
`~/.trusty-voice/speaker-profile.pv`; main loop activates on "Hey Trusty" + speaker
verification pass; PTT remains as a fallback.

#### Phase 2 — Streaming SSE `coordinator_chat` variant

**Goal:** true speak-while-generating — TTS begins as the first agent reply tokens arrive.

**Required backend change:** a new SSE-capable variant of `POST /api/v1/sessions/chat` (or
a new endpoint `GET /api/v1/sessions/chat/stream`) that emits reply chunks as they are
generated by the agent. The trusty-memory daemon's streaming chat handler
(approximately `lib.rs:1925`) is the reference pattern.

**Voice client change:** Pipecat pipeline switches from collecting a full JSON reply to
consuming an SSE stream; TTS synthesis begins on the first chunk.

**Deliverable:** streaming `coordinator_chat` backend route + updated Pipecat processor.

#### Phase 3 — Token-auth gateway + Linux SFF WebRTC thin client

**Goal:** a dedicated always-on voice terminal (Raspberry Pi 5 or N100 mini-PC) that
offloads all processing to the HOST.

**Backend:** token-auth gateway in front of coordinator endpoints
(§SPEC-VOICE-07~draft); Pipecat WebRTC transport enabled on the HOST.

**SFF client:** Chromium kiosk page capturing mic via `getUserMedia`, streaming over
WebRTC to the HOST Pipecat pipeline, playing back TTS audio. The Svelte embedded-UI
pattern from the workspace (trusty-search / trusty-memory UI builds) is a candidate
starting point for the kiosk page.

**Deliverable:** gateway implementation; Pipecat WebRTC server config; kiosk HTML/JS page.

### Productization question (non-contractual)

As trusty-voice grows beyond a prototype, a design decision arises: keep the Python Pipecat
service as the cross-platform voice engine, or rewrite the pipeline in Rust to align with
the rest of the workspace.

**Recommendation for v1 and the foreseeable future: keep Python + Pipecat.**

The voice ecosystem (Deepgram, ElevenLabs, Picovoice, WebRTC) is significantly more mature
on Python than on Rust. Pipecat's pipeline model, interruption support, and existing service
integrations provide months of accelerated development. A Rust reimplementation would require
reproducing all of these from scratch with no ecosystem leverage. Revisit this question when
v1 is validated and the product direction is clearer.

### Rationale (WHY)

Phasing the roadmap as Phase 0 (no Picovoice) → Phase 1 (wake word + verification) → Phase 2
(streaming) → Phase 3 (SFF remote) allows the core voice loop to be validated end-to-end
before any external-device or complex-backend work begins. Phase 0 can be built and tested
in an afternoon; it is the fastest path to a working demo that proves the `coordinator_chat`
binding works over voice.
