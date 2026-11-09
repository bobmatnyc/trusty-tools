# tm-agents: Assistants App — Product Brief

**Date:** 2026-10-29  
**Owner:** Bob  
**PM:** tm-agents session

## Context

Owner direction (2026-10-29 21:0xZ):
- "Let's go back to the app UI, but simplify it. The model is the same, we can create multiple assistants, each with an indexed knowledge store, chat is perpetual, runs in an app"
- "Somewhere between ChatGPT and Muse. Like ChatGPT we can create multiple assistants, like Muse the UI is simpler."

Follow-up ruling (2026-10-29 21:08Z):
- Prototype in Python; Rust port later when functionality is stable
- Use AWS Bedrock for all inference
- Track spending (tokens + cost per assistant, reconcile with AWS bill)

## Vision

A simplified desktop app for managing multiple AI assistants, each with its own:
- Name and instructions
- Indexed knowledge store (documents via trusty-search, facts via trusty-memory)
  - Note (2026-11-03): superseded. Owner definition: Assistant knowledge uses the trusty-okf store (SPEC-OKF-01, not yet accepted), separate from trusty-search and trusty-memory, and applies only to Assistants. See "Assistant: definition and knowledge store" in docs/briefs/2026-10-29-tm-agents-epic-proposal.md. The trusty-search/trusty-memory knowledge items below (lines ~50-51, Phase 3) are not to be built as written.
- Perpetual conversation (not throwaway threads)
- Spending history (tokens + estimated cost)

Design inspiration:
- **ChatGPT's model:** Multiple named assistants, each a distinct persona
- **Muse's UX:** Messaging-first interface (feels like WhatsApp), no rigid turn-taking, chat bubbles for visual separation
- **Codex reference:** Agent-native interface with tools built in (though our MVP keeps it simple)

## Current State Inventory

### What Exists Today

**UI (crates/trusty-agents/ui)**:
- Tauri 2 + Svelte 4 desktop app
- 48 Svelte components (~9,277 lines)
- Complex features:
  - Fine-grained agent configuration (coding, knowledge, permissions, skills, subagents)
  - Multi-channel routing (Slack mirror, channels view, global channels panel)
  - Project-scoped sessions with attach/resume/pause
  - File navigator and viewer
  - Events/workflow telemetry view
  - MCP connection configuration
  - Knowledge pipeline configuration

**Backend (crates/trusty-agents Rust)**:
- Rust harness with subprocess delegation
- OpenRouter, Anthropic direct, AWS Bedrock providers already integrated
- trusty-search integration for code/doc indexing
- trusty-memory integration for facts
- API server with SSE event streaming

**AWS Bedrock Setup** (D9f ruling 2026-10-29):
- Profile: `cto` (default) - **must be a config setting, never hard-coded** (AWS_PROFILE env var or config file)
- Region: `us-east-1` (AWS_REGION=us-east-1)
- Available models (verified 2026-10-29):
  - anthropic.claude-opus-5-5
  - anthropic.claude-sonnet-5-5
  - anthropic.claude-haiku-5-5
  - anthropic.claude-fable-5
  - (plus older versions)

## UI Model: Intent Ledger for Assistants

### Concept

Each assistant maintains an **Intent Ledger**-style record alongside its perpetual chat:
- **Goals:** What the assistant is working toward
- **Tasks:** What it is currently doing
- **Questions:** What it asked and what it needs from the user
- **Decisions:** What it decided and why

The UI presents this in **plain language** — no technical jargon (no "ledger", "digest", "schema", "id"). Each item shows current state in user-friendly terms.

### How It Works

**Chat turns create and update items:**

1. **Assistant sets a goal** (from chat):
   - User: "Help me plan a trip to Japan"
   - Assistant: "I'll help you plan your Japan trip. Let me start by understanding your timeline and preferences."
   - **Goal created:** "Plan a trip to Japan" (shown in Goals section)

2. **Assistant creates tasks** (from chat):
   - Assistant: "I'll research: (1) visa requirements, (2) best season to visit, (3) recommended cities"
   - **Tasks created:** 
     - "Research visa requirements"
     - "Find best season to visit"
     - "Recommend cities to visit"

3. **Assistant asks a question** (from chat):
   - Assistant: "When are you planning to travel? Also, do you prefer cities or countryside?"
   - **Questions created:**
     - "When are you planning to travel?" (status: waiting for answer)
     - "Do you prefer cities or countryside?" (status: waiting for answer)

4. **User answers** (in Questions section OR in chat):
   - User clicks "Answer" button next to question, types "March 2027, I prefer cities"
   - OR user replies in chat: "March 2027, and I prefer cities"
   - **Questions updated:** both marked answered, answers stored

5. **Assistant makes a decision** (from chat):
   - Assistant: "Based on your March timeline and city preference, I recommend Tokyo and Kyoto. March is cherry blossom season, so book early."
   - **Decision created:** "Recommended Tokyo and Kyoto for March 2027 (cherry blossom season)" (shown in Decisions section)

6. **User edits a goal/task/decision** (in UI):
   - User clicks "Edit" next to goal "Plan a trip to Japan", changes to "Plan a 10-day trip to Japan"
   - **Goal updated**, assistant sees the edit in next turn

**Items persist** across sessions. Completed tasks move to a "Done" section. Answered questions stay visible with their answers.

### Layout Options

**Option 1: Side Rail (Recommended)**

```
┌─────────────────────┬─────────────────────────────────────┐
│ Assistants          │ Chat: Trip Planner                  │
│ • Trip Planner   [▼]│ ──────────────────────────────────  │
│ • Code Helper       │                                     │
│ • Researcher        │ User: Help me plan a trip to Japan  │
│                     │                                     │
│ ──── Trip Planner ──│ Asst: I'll help you plan your trip. │
│                     │ Let me start by understanding your  │
│ Goals (1)           │ timeline and preferences.           │
│ ▸ Plan a 10-day trip│                                     │
│   to Japan          │ Asst: When are you planning to      │
│   [Edit]            │ travel? Do you prefer cities or     │
│                     │ countryside?                        │
│ Tasks (3)           │                                     │
│ ☐ Research visa req │ User: March 2027, I prefer cities   │
│ ☐ Best season       │                                     │
│ ☐ Recommend cities  │ ────────────────────────────────── │
│                     │                                     │
│ Questions (2)       │ [Type a message...]          [Send] │
│ ✓ When traveling?   │                                     │
│   → March 2027      │                                     │
│ ✓ Cities or country?│                                     │
│   → Cities          │                                     │
│                     │                                     │
│ Decisions (1)       │                                     │
│ • Tokyo & Kyoto for │                                     │
│   March (cherry...  │                                     │
│   [View]            │                                     │
└─────────────────────┴─────────────────────────────────────┘
```

**Why recommended:**
- Always visible alongside chat (no tab switching)
- Collapsible sections keep it compact
- Clear separation: assistant list + active assistant's ledger
- Chat takes center stage (primary interaction)

---

**Option 2: Top Tabs**

```
┌────────────────────────────────────────────────────────────┐
│ Trip Planner  [Chat] [Goals] [Tasks] [Questions] [Decisions│
├────────────────────────────────────────────────────────────┤
│                                                            │
│ User: Help me plan a trip to Japan                         │
│                                                            │
│ Asst: I'll help you plan your trip. Let me start by        │
│ understanding your timeline and preferences.               │
│                                                            │
│ Asst: When are you planning to travel? Do you prefer       │
│ cities or countryside?                                     │
│                                                            │
│ User: March 2027, I prefer cities                          │
│                                                            │
│ ──────────────────────────────────────────────────────────│
│                                                            │
│ [Type a message...]                               [Send]   │
└────────────────────────────────────────────────────────────┘

(Clicking [Goals] tab shows:)
┌────────────────────────────────────────────────────────────┐
│ Trip Planner  [Chat] [Goals·] [Tasks] [Questions] [Decisions│
├────────────────────────────────────────────────────────────┤
│                                                            │
│ Goals                                          [+ New Goal]│
│                                                            │
│ ┌──────────────────────────────────────────────────────┐  │
│ │ Plan a 10-day trip to Japan                    [Edit]│  │
│ │ Created: Oct 29, 2026                                │  │
│ └──────────────────────────────────────────────────────┘  │
│                                                            │
└────────────────────────────────────────────────────────────┘
```

**Tradeoffs:**
- More screen space for each section when viewed
- Requires tab switching to see goals/tasks/questions
- Chat and ledger items compete for attention

---

**Option 3: Inline Cards in Chat**

```
┌────────────────────────────────────────────────────────────┐
│ Chat: Trip Planner                                [Compact]│
├────────────────────────────────────────────────────────────┤
│                                                            │
│ User: Help me plan a trip to Japan                         │
│                                                            │
│ Asst: I'll help you plan your trip.                        │
│                                                            │
│ ┌────────────────────────────────────────────────────┐    │
│ │ 🎯 Goal: Plan a 10-day trip to Japan        [Edit] │    │
│ └────────────────────────────────────────────────────┘    │
│                                                            │
│ Asst: When are you planning to travel?                     │
│                                                            │
│ ┌────────────────────────────────────────────────────┐    │
│ │ ❓ When are you planning to travel?       [Answer] │    │
│ │ ❓ Do you prefer cities or countryside?   [Answer] │    │
│ └────────────────────────────────────────────────────┘    │
│                                                            │
│ User: March 2027, I prefer cities                          │
│                                                            │
│ ┌────────────────────────────────────────────────────┐    │
│ │ ✅ Answered: March 2027, cities                     │    │
│ └────────────────────────────────────────────────────┘    │
│                                                            │
│ [Type a message...]                               [Send]   │
└────────────────────────────────────────────────────────────┘
```

**Tradeoffs:**
- Ledger items visible in context (where they were created)
- No separate navigation needed
- Chat becomes cluttered with cards
- Harder to see all goals/tasks at once

---

### Recommendation: Option 1 (Side Rail)

**Rationale:**
- Balances visibility (always on screen) with focus (chat is primary)
- No tab switching friction
- Collapsed sections keep it compact when not needed
- Easy to scan current state (all goals, all tasks) at a glance
- Follows Muse's simplicity principle while adding structure

**Implementation note:** The side rail is collapsible (▸/▼ toggle) so users can hide it for full-screen chat when ledger items aren't relevant.

### Changes to Phase 1 Scope

**Added to Phase 1:**

1. **Data model extensions:**
   - `goals` table: `id`, `assistant_id`, `content`, `status` (active/completed), `created_at`, `updated_at`
   - `tasks` table: `id`, `assistant_id`, `goal_id` (optional), `content`, `status` (pending/done), `created_at`, `completed_at`
   - `questions` table: `id`, `assistant_id`, `question`, `answer`, `status` (unanswered/answered), `created_at`, `answered_at`
   - `decisions` table: `id`, `assistant_id`, `content`, `rationale`, `created_at`

2. **Bedrock prompt engineering:**
   - System prompt instructs assistant to explicitly state goals, tasks, questions, decisions
   - Response parsing extracts structured items (regex or simple heuristics: "Goal:", "Task:", "Question:", "Decision:")
   - Example prompt addition:
     ```
     When setting a goal, start a line with "Goal: ..."
     When creating a task, start a line with "Task: ..."
     When asking a question, start a line with "Question: ..."
     When making a decision, start a line with "Decision: ..."
     ```

3. **UI components (Svelte):**
   - `GoalsPanel.svelte` (list goals, edit button, mark complete)
   - `TasksPanel.svelte` (list tasks, checkboxes, mark done)
   - `QuestionsPanel.svelte` (list questions, answer input, show answers)
   - `DecisionsPanel.svelte` (list decisions, view rationale)
   - `SideRail.svelte` (container for all panels, collapsible sections)

4. **API endpoints:**
   - `GET /assistants/{id}/goals`, `POST /assistants/{id}/goals`, `PATCH /assistants/{id}/goals/{goal_id}`
   - `GET /assistants/{id}/tasks`, `POST /assistants/{id}/tasks`, `PATCH /assistants/{id}/tasks/{task_id}`
   - `GET /assistants/{id}/questions`, `POST /assistants/{id}/questions`, `PATCH /assistants/{id}/questions/{q_id}` (answer endpoint)
   - `GET /assistants/{id}/decisions`, `POST /assistants/{id}/decisions`

5. **Chat integration:**
   - After each assistant response, parse for "Goal:", "Task:", "Question:", "Decision:" markers
   - Auto-create corresponding items in DB
   - Show inline indicators in chat when items are created (e.g., small badge "1 new goal")
   - When user answers a question in chat, detect and update the question item

**Removed from Phase 1:**
- None (this is additive)

**Phase 1 timeline impact:**
- Original: 10 slices, ~2 weeks
- With UI model: +3 slices (ledger data model, response parsing, side rail UI) = **13 slices, ~2.5 weeks**

**New Phase 1 slices (added):**

- **Slice 1.11: Ledger Data Model + Parsing**
  - Create goals/tasks/questions/decisions tables
  - Write response parser: extract items from assistant text
  - Unit tests for parsing heuristics

- **Slice 1.12: Ledger API Endpoints**
  - CRUD endpoints for goals, tasks, questions, decisions
  - Auto-create items after each chat turn (call parser)

- **Slice 1.13: Side Rail UI**
  - `SideRail.svelte` with collapsible sections
  - Goals/Tasks/Questions/Decisions panels
  - Edit/Answer/Mark-done interactions
  - Sync with API on mount and after chat turns

**Acceptance criteria (added to Phase 1 QA):**
- [ ] Chat turn "Goal: Plan a trip" creates a goal item in side rail
- [ ] User edits goal in side rail, next chat turn reflects the edit
- [ ] Chat turn with "Question: When are you traveling?" creates question item
- [ ] User clicks "Answer" button, types answer, question marked answered
- [ ] Completed tasks move to "Done" section, stay visible
- [ ] All ledger items persist across app restarts

---

**Summary of UI Model Impact:**

| Aspect | Before | After |
|---|---|---|
| **Data captured** | Chat messages only | Messages + structured goals/tasks/questions/decisions |
| **UI layout** | Chat + header | Chat + side rail with ledger panels |
| **User interaction** | Type in chat only | Chat + direct ledger item editing |
| **Phase 1 duration** | ~2 weeks | ~2.5 weeks |
| **Phase 1 slices** | 10 | 13 |

The Intent Ledger model keeps the Muse-like simplicity (messaging-first, no rigid workflows) while adding structure that helps users track what the assistant is working on and what it needs from them.

## MVP Scope

### Phase 1: Python Prototype (Core Chat + Intent Ledger)

**In scope:**
1. **Assistant management**
   - Create/rename/delete assistants
   - Each assistant has: name, system instructions, model choice (fixed per assistant, not configurable per-message)
   - Assistant list in sidebar
   - Active assistant selection persists across app restarts

2. **Perpetual chat**
   - One continuing conversation per assistant
   - Message history persisted locally (SQLite or JSON)
   - Markdown rendering for messages
   - Simple chat bubbles (user/assistant visual separation)
   - No turn-taking requirement (can send multiple messages)

3. **AWS Bedrock inference**
   - All LLM calls via AWS Bedrock
   - Default model: `us.anthropic.claude-sonnet-5-5` (inference profile, not bare model ID)
   - Profile: `cto`, Region: `us-east-1`
   - Token streaming for responsive UX
   - **Note:** Bedrock rejects bare model IDs; use inference profiles (`us.*` or `global.*`)

4. **Desktop app**
   - Electron or Tauri (Tauri preferred for consistency with existing codebase, but Electron acceptable if Python integration is simpler)
   - macOS initially (cross-platform later)
   - System tray icon
   - Persistent window state

**Out of scope (Phase 1):**
- Knowledge integration (trusty-search, trusty-memory) — **Phase 3**
- Spending tracking — **Phase 2**
- Multi-channel routing
- Project-scoped sessions
- File browsing
- Workflow events view
- MCP configuration UI
- Agent configuration beyond name + instructions + model

### Phase 2: Spending Tracking

**In scope:**
1. **Per-call tracking**
   - Capture input/output tokens from Bedrock response metadata
   - Estimate cost per call using published Bedrock pricing:
     - Sonnet 4.6: $3/MTok input, $15/MTok output
     - Opus 5.5: $15/MTok input, $75/MTok output
     - Haiku 5.5: $0.80/MTok input, $4/MTok output
   - Store: timestamp, assistant_id, model, input_tokens, output_tokens, estimated_cost_usd

2. **Assistant-level view**
   - Total spend per assistant (all-time, this month, this week)
   - Message count
   - Token totals
   - Simple table or cards in UI

3. **AWS bill reconciliation**
   - Export spending data as CSV
   - Column: assistant_name, model, date, tokens_in, tokens_out, estimated_cost
   - User manually compares against AWS Cost Explorer filtered by `bedrock-runtime` service
   - **Not automated reconciliation** (that requires AWS Cost API + tagging, too complex for Phase 2)

**Out of scope (Phase 2):**
- Real-time AWS bill integration
- Budget alerts
- Cost optimization recommendations

### Phase 3: Knowledge Integration

**In scope:**
1. **Document indexing (trusty-search)**
   - Per-assistant document store
   - Upload files (PDF, markdown, code) via drag-drop
   - Indexed with trusty-search (vector + lexical)
   - Retrieved context injected into chat on demand ("search my docs for X")

2. **Fact memory (trusty-memory)**
   - Per-assistant memory namespace
   - Facts extracted from conversation
   - Recalled automatically in relevant turns

**Integration strategy:**
- Python bindings to Rust daemons (trusty-search, trusty-memory run as separate processes)
- Communication via Unix domain sockets (existing trusty-memory protocol)
- Or: HTTP/JSON-RPC if socket binding is complex

**Out of scope (Phase 3):**
- Knowledge graph browser UI
- Memory fan-out configuration
- Cross-assistant knowledge sharing

### Phase 4: Rust Port (Stability Gate)

**Gate to enter Phase 4:**
- Phase 1–3 behaviours stable for 2+ weeks
- No major UX changes requested
- Core workflows validated by daily use

**In scope:**
1. Port Python prototype to Rust
2. Reuse existing trusty-agents crates where applicable
3. Maintain UI compatibility (same Tauri frontend)
4. Performance optimization (startup time, memory footprint)

**Out of scope (Phase 4):**
- New features (feature-freeze during port)

## Cut List from Current UI

**Components to remove:**
- AgentConfigCoding, AgentConfigKnowledge, AgentConfigPermissions, AgentConfigSkills, AgentConfigSubagents (fine-grained config)
- AssistantKnowledgePipeline, AssistantMcpConnections (advanced integrations — moved to Phase 3, simplified)
- ChannelsView, GlobalChannelsPanel, AssistantRoutedChannels (multi-channel)
- EventsView (workflow telemetry)
- FileNavigator, FileViewer (file browsing — not needed for chat-only MVP)
- AssistantProjectSettings (project-scoped sessions)
- ChatAttachments (defer to Phase 3 if needed for docs upload)
- CostsView (rebuilt in Phase 2 with spending tracking)

**Components to keep (simplified):**
- AssistantPicker (choose assistant, create new)
- ChatView/ChatPane (main conversation)
- ChatHeader (assistant name, settings icon)
- Header (top nav, minimal)
- Sidebar (assistant list only)
- Simple settings panel: assistant name, instructions, model choice

## Architecture (Phase 1)

### Tech Stack

**Frontend:**
- Svelte 5 (upgrade from 4, or keep 4 if migration is risky)
- Tailwind CSS (existing styles)
- Tauri 2 (Rust desktop shell, reuse existing trusty-agents/ui/src-tauri structure)

**Backend:**
- Python 3.12+
- FastAPI or Flask for local HTTP server (similar to existing `tagent --api`)
- boto3 for AWS Bedrock client
- SQLite for local message/assistant persistence
- uvicorn for async server

**Data Model:**

```python
# assistants table
CREATE TABLE assistants (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    instructions TEXT,
    model TEXT DEFAULT 'us.anthropic.claude-sonnet-5-5',
    created_at TIMESTAMP,
    updated_at TIMESTAMP
);

# messages table
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    assistant_id TEXT REFERENCES assistants(id),
    role TEXT CHECK(role IN ('user', 'assistant')),
    content TEXT NOT NULL,
    timestamp TIMESTAMP,
    tokens_in INTEGER,
    tokens_out INTEGER
);
```

### AWS Bedrock Integration

**Authentication** (D9f ruling 2026-10-29):
- Use existing AWS credentials (profile `cto` default)
- **Profile name is a config setting** (AWS_PROFILE env var or app config), never hard-coded
- boto3 auto-detects from `~/.aws/credentials` and `AWS_PROFILE` env var
- Rationale: allows switching between `cto` (Duetto dev) and `1M-OP` (1MC) accounts without code changes

**Python client example:**

```python
import boto3
from botocore.config import Config

bedrock = boto3.client(
    service_name='bedrock-runtime',
    region_name='us-east-1',
    config=Config(
        profile_name='cto',
        retries={'max_attempts': 3, 'mode': 'standard'}
    )
)

response = bedrock.converse_stream(
    modelId='us.anthropic.claude-sonnet-5-5',  # inference profile, not bare model ID
    messages=[
        {'role': 'user', 'content': [{'text': 'Hello'}]}
    ]
)

for event in response['stream']:
    if 'contentBlockDelta' in event:
        print(event['contentBlockDelta']['delta']['text'], end='')
```

**What's missing (gaps to fill):**
- Python environment setup (requirements.txt with boto3, fastapi, sqlite)
- Tauri → Python IPC bridge (HTTP localhost, similar to existing `tagent --api` sidecar pattern)
- Error handling for Bedrock throttling/quota (exponential backoff via boto3 retry config)

### Spending Tracking (Phase 2 Detail)

**Bedrock response metadata:**

```python
response = bedrock.converse(...)
usage = response['usage']
# usage['inputTokens'], usage['outputTokens']
```

**Pricing table (embedded in code):**

```python
BEDROCK_PRICING = {
    'us.anthropic.claude-sonnet-5-5': {'input': 3.0, 'output': 15.0},  # per MTok
    'us.anthropic.claude-opus-5-5': {'input': 15.0, 'output': 75.0},
    'us.anthropic.claude-haiku-5-5': {'input': 0.80, 'output': 4.0},
}

def estimate_cost(model_id, input_tokens, output_tokens):
    pricing = BEDROCK_PRICING.get(model_id, {'input': 0, 'output': 0})
    return (input_tokens / 1_000_000 * pricing['input']) + \
           (output_tokens / 1_000_000 * pricing['output'])
```

**Storage:**

```python
# spending table (Phase 2)
CREATE TABLE spending (
    id TEXT PRIMARY KEY,
    assistant_id TEXT REFERENCES assistants(id),
    message_id TEXT REFERENCES messages(id),
    model TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    estimated_cost_usd REAL,
    timestamp TIMESTAMP
);
```

**CSV export for reconciliation:**

```python
SELECT 
    a.name as assistant_name,
    s.model,
    DATE(s.timestamp) as date,
    SUM(s.input_tokens) as tokens_in,
    SUM(s.output_tokens) as tokens_out,
    SUM(s.estimated_cost_usd) as estimated_cost
FROM spending s
JOIN assistants a ON s.assistant_id = a.id
GROUP BY a.name, s.model, DATE(s.timestamp)
ORDER BY date DESC;
```

User downloads CSV, opens AWS Cost Explorer, filters by `bedrock-runtime` service for the same date range, compares totals.

## Open Questions

### 1. Python vs. keeping Rust for prototype?

**Recommendation: Python prototype (per owner ruling)**

*Rationale:*
- Faster iteration on UX (no compile time)
- Easier to pivot if requirements change
- Existing Rust codebase is complex (subprocess delegation, multi-provider routing) — overkill for simplified app
- boto3 is mature and well-documented
- Gate to Rust port ensures we don't prematurely optimize

*Tradeoff:*
- Two implementations to maintain during transition
- Python adds runtime dependency

**Gate to Rust port:** 2+ weeks stable use, no major UX changes, core workflows validated.

### 2. Electron vs. Tauri for desktop shell?

**Recommendation: Tauri (reuse existing crates/trusty-agents/ui/src-tauri)**

*Rationale:*
- Existing trusty-agents UI is Tauri 2 — reuse sidecar spawn pattern
- Smaller binary size (~10MB vs ~100MB for Electron)
- Rust sidecar launcher is already debugged
- Consistent with existing codebase

*Tradeoff:*
- Python ↔ Tauri IPC is HTTP (same as current `tagent --api`), not native Tauri commands
- If Python port proves difficult, Electron's `child_process.spawn` might be simpler

**Decision point:** Prototype with Tauri first. If Python sidecar spawn is problematic, pivot to Electron.

### 3. Knowledge integration: when and how much?

**Recommendation: Phase 3, after spending tracking**

*Rationale:*
- Owner wants "indexed knowledge store" but simplicity is paramount
- Phase 1 proves chat UX without complexity
- Phase 2 proves cost tracking (table stakes for production use)
- Phase 3 adds knowledge when base is stable

*Minimal scope for Phase 3:*
- Drag-drop file upload
- "Search my docs" command in chat
- Auto-recall facts from memory
- No UI for knowledge graph browsing (defer indefinitely)

### 4. Message context window management?

**Recommendation: Simple truncation initially, compaction later**

*Phase 1 approach:*
- Keep last N messages (e.g., 50) in context
- Older messages stay in DB but drop out of LLM context
- Warning when approaching limit

*Phase 3 enhancement (with memory):*
- Summarize old turns into memory facts
- Keep recent messages + recalled facts in context
- trusty-memory handles compaction

### 5. Multi-assistant vs. single with personas?

**Recommendation: Multi-assistant (per owner direction)**

*Rationale:*
- Owner explicitly said "multiple assistants like ChatGPT"
- Each assistant = isolated namespace (own memory, own knowledge, own spend tracking)
- Simpler to reason about than persona-switching within one conversation

*Tradeoff:*
- More DB tables (assistants + foreign keys)
- Sidebar UI for switching

**Confirmed:** Multi-assistant model.

## Success Criteria

**Phase 1 (Python prototype):**
- [ ] Create 3 assistants with different instructions
- [ ] Chat with each assistant, verify conversation persists across app restarts
- [ ] Verify Bedrock streaming works (messages appear token-by-token)
- [ ] Verify system tray icon and window state persistence
- [ ] Daily use by owner for 1 week without crashes

**Phase 2 (Spending tracking):**
- [ ] Every message shows token counts
- [ ] Assistant view shows total spend per assistant
- [ ] Export CSV and manually reconcile with AWS Cost Explorer for 1 month
- [ ] Estimated cost within 5% of actual AWS bill

**Phase 3 (Knowledge integration):**
- [ ] Upload 10 PDFs to an assistant's knowledge store
- [ ] "Search my docs for X" returns relevant excerpts
- [ ] Memory recalls prior conversation facts without explicit search
- [ ] Knowledge isolated per assistant (no cross-contamination)

**Phase 4 (Rust port):**
- [ ] Rust binary functionally equivalent to Python prototype
- [ ] All Phase 1–3 tests pass
- [ ] Startup time <2s (vs. Python's ~5s)
- [ ] Memory footprint <100MB (vs. Python's ~200MB)

## Next Steps

1. **Owner review of this brief** — confirm Phase 1 scope, gate to Rust port, spending tracking approach
2. **Epic proposal** — break Phase 1 into ~5 implementation slices (see separate document)
3. **Python env setup** — requirements.txt, local dev instructions
4. **Tauri sidecar scaffold** — adapt existing ui/src-tauri to spawn Python server instead of `tagent`
5. **First slice: assistant CRUD + empty chat view** — prove the data model and UI shell

## References

- Original brief: `/Users/masa/trusty-mpm-projects/architect/local/briefs/2026-10-29-tm-agents-app-brief.md`
- Research findings: Codex (OpenAI 2025), Muse (Meta 2026)
- Existing codebase: `crates/trusty-agents` (Rust harness, UI)
- AWS Bedrock models: `us.anthropic.claude-sonnet-5-5` (default), `us.anthropic.claude-opus-5-5` (high-capability), `us.anthropic.claude-haiku-5-5` (fast/cheap)
