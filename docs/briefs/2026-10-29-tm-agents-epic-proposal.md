# tm-agents Assistants App — Epic Proposal

**Date:** 2026-10-29  
**Owner:** Bob  
**PM:** tm-agents session  
**Brief:** `docs/briefs/2026-10-29-tm-agents-assistants-app.md`

## Assistant: definition and knowledge store (owner, 2026-11-03)

- An Assistant is the personal-assistant product entity in Trusty Agents, comparable to Dots or Grok bots. The comparison is a positioning reference, not a mandate to copy features.
- The trusty-okf knowledge-store requirement applies only to Assistants, not to coding or engineering agents.
- The Assistant knowledge store is separate from trusty-memory and trusty-search. Assistant knowledge does not route through either.
- Dependency: SPEC-OKF-01 (`/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-okf/docs/specs/SPEC-OKF-01.md`, trusty-okf repo, Draft r5, not accepted). No knowledge-store integration is built until it is accepted. Priority: low.

Phase 3 below predates this ruling; see the note there.

---

## Epic Structure

This epic is split into **4 phases**, with each phase delivering a shippable increment. Phases 1–3 are feature-driven; Phase 4 is the Rust port gated on stability.

```
Phase 1: Python Prototype (Core Chat)
    ↓ (gate: daily use for 1 week, no crashes)
Phase 2: Spending Tracking
    ↓ (gate: reconcile with AWS bill for 1 month)
Phase 3: Knowledge Integration
    ↓ (gate: 2+ weeks stable, validated workflows)
Phase 4: Rust Port
```

---

## Phase 1: Python Prototype (Core Chat)

**Goal:** Shippable desktop app with multi-assistant chat via AWS Bedrock, Intent Ledger UI (goals/tasks/questions/decisions), and per-call spend counter.

**Gate to Phase 2:** Daily use by owner for 1 week without crashes.

### Slices

#### Slice 1.1: Python Environment + Bedrock Smoke Test

**Deliverable:** Python script that calls Bedrock and prints streaming response.

**Tasks:**
- [ ] Create `assistants-app/` directory (peer to `crates/`)
- [ ] Write `requirements.txt` (boto3, fastapi, uvicorn, sqlite)
- [ ] Write `smoke_test.py`: call `bedrock.converse_stream` with `us.anthropic.claude-sonnet-5-5` (inference profile), print tokens
- [ ] Verify AWS credentials work (profile `cto`, region `us-east-1`)
- [ ] Document setup in `assistants-app/README.md`

**Acceptance:**
- `python smoke_test.py` streams "Hello, how can I help?" from Bedrock
- No errors, tokens print to stdout

---

#### Slice 1.2: Data Model + Assistant CRUD

**Deliverable:** SQLite schema + Python functions to create/read/update/delete assistants.

**Tasks:**
- [ ] Write `assistants-app/schema.sql` (assistants, messages tables)
- [ ] Write `assistants-app/db.py`: init_db(), create_assistant(), list_assistants(), get_assistant(), update_assistant(), delete_assistant()
- [ ] Write unit tests (pytest) for CRUD functions
- [ ] Seed script: create 2 default assistants ("General", "Code Helper")

**Acceptance:**
- `pytest assistants-app/tests/` passes
- `python seed.py` creates DB with 2 assistants
- `sqlite3 assistants.db "SELECT * FROM assistants"` shows 2 rows

---

#### Slice 1.3: FastAPI Server + Chat Endpoint

**Deliverable:** HTTP server with POST `/chat` endpoint that calls Bedrock and stores messages.

**Tasks:**
- [ ] Write `assistants-app/server.py`: FastAPI app
- [ ] Endpoint: `POST /assistants` (create)
- [ ] Endpoint: `GET /assistants` (list)
- [ ] Endpoint: `GET /assistants/{id}` (get one)
- [ ] Endpoint: `POST /assistants/{id}/chat` (send message, call Bedrock, store user + assistant messages, return response)
- [ ] Endpoint: `GET /assistants/{id}/messages` (get message history)
- [ ] Handle Bedrock errors (throttling, quota) with exponential backoff

**Acceptance:**
- `uvicorn server:app --reload` starts server on `localhost:8000`
- `curl -X POST localhost:8000/assistants/123/chat -d '{"content":"Hello"}'` returns streamed JSON deltas
- Messages appear in DB

---

#### Slice 1.4: Tauri Shell + Sidecar Launch

**Deliverable:** Tauri app that spawns Python server on startup.

**Tasks:**
- [ ] Copy `crates/trusty-agents/ui/src-tauri` to `assistants-app/tauri`
- [ ] Strip out `tagent` sidecar code, replace with Python sidecar
- [ ] Update `tauri.conf.json`: sidecar command = `["python3", "server.py"]`
- [ ] Test: launch Tauri app, verify Python server starts and responds to `GET /assistants`
- [ ] Handle sidecar crash: show error dialog, offer to restart

**Acceptance:**
- Tauri app launches without errors
- Activity Monitor shows `python3 server.py` process
- Killing Python process triggers error dialog in app

---

#### Slice 1.5: Svelte UI — Assistant Picker

**Deliverable:** Landing screen with assistant cards + "Create new" button.

**Tasks:**
- [ ] Strip down `crates/trusty-agents/ui/src/App.svelte`: remove ChannelsView, CostsView, EventsView, FileViewer
- [ ] Simplify `AssistantPicker.svelte`: show cards with name only (no config preview)
- [ ] "Create new" button opens modal: name input + "Create" button
- [ ] Clicking a card sets `activeAssistantId` and navigates to Chat view
- [ ] Persist `activeAssistantId` in localStorage

**Acceptance:**
- App opens showing 2 assistant cards ("General", "Code Helper")
- Clicking "Create new", entering "Researcher", clicking Create adds a third card
- Clicking "General" card navigates to Chat view (empty for now)
- Reopening app shows all 3 assistants

---

#### Slice 1.6: Svelte UI — Chat View

**Deliverable:** Chat view with message bubbles, input area, send button.

**Tasks:**
- [ ] Simplify `ChatView.svelte`: remove events panel, recap rail, Slack mirror
- [ ] `ChatHeader.svelte`: show assistant name + back button (returns to AssistantPicker)
- [ ] `InputArea.svelte`: textarea + Send button
- [ ] On Send: `POST /assistants/{id}/chat`, stream response into new assistant bubble
- [ ] Render user messages as right-aligned bubbles, assistant as left-aligned
- [ ] Markdown rendering for assistant messages (marked.js + highlight.js)
- [ ] Auto-scroll to bottom on new message

**Acceptance:**
- Select "General" assistant, type "Hello", click Send
- User bubble appears (right, blue)
- Assistant bubble appears token-by-token (left, gray), fully rendered with markdown
- Sending 5 messages shows 10 bubbles total, scroll at bottom
- Switching to "Code Helper" shows empty history (isolated per assistant)

---

#### Slice 1.7: Message Persistence + History Load

**Deliverable:** Messages persist across app restarts.

**Tasks:**
- [ ] On Chat view mount: `GET /assistants/{id}/messages`, render history
- [ ] Show loading spinner while fetching
- [ ] Handle empty history (show "Start a conversation" placeholder)
- [ ] Handle large history (limit to last 50 messages initially, "Load more" later)

**Acceptance:**
- Send 3 messages to "General", close app
- Reopen app, select "General", see 6 bubbles (3 user + 3 assistant)
- Messages appear in correct order with original timestamps

---

#### Slice 1.8: Settings Panel (Assistant Name + Instructions)

**Deliverable:** Gear icon in ChatHeader opens settings overlay.

**Tasks:**
- [ ] `ChatHeader.svelte`: add gear icon (lucide-svelte `Settings`)
- [ ] Click gear → show `AssistantSettings.svelte` overlay (modal covering chat)
- [ ] Form: name input, instructions textarea, model dropdown (hardcoded to Sonnet 5.5 for Phase 1)
- [ ] Save button: `PATCH /assistants/{id}`, close overlay, update header name
- [ ] Cancel button: discard changes, close overlay

**Acceptance:**
- Click gear on "General", change name to "General Assistant", save
- Header updates to "General Assistant"
- Click gear, change instructions to "Be concise", save
- Send message, verify response is concise (qualitative check)
- Reopen app, verify name and instructions persisted

---

#### Slice 1.9: System Tray + Window State

**Deliverable:** Tray icon, hide-to-tray, restore window, quit.

**Tasks:**
- [ ] Add tray icon (reuse `ui/src-tauri/icons/tray-icon.png`)
- [ ] Tray menu: "Show", "Quit"
- [ ] Closing window hides to tray (doesn't quit)
- [ ] Tray "Show" restores window
- [ ] Tray "Quit" kills Python sidecar and exits app
- [ ] Persist window position/size in localStorage

**Acceptance:**
- Launch app, close window (X button), verify it hides (not in Dock)
- Tray icon visible in menu bar
- Click tray icon → "Show" → window restores
- Click tray → "Quit" → app exits, Python process killed

---

#### Slice 1.10: Per-Call Spend Counter (moved from Phase 2)

**Deliverable:** Log input/output tokens and estimated cost for every message.

**Tasks:**
- [ ] Update `server.py`: extract `usage['inputTokens']` and `usage['outputTokens']` from Bedrock response
- [ ] Add `tokens_in`, `tokens_out` columns to `messages` table
- [ ] Embed pricing table: Sonnet 5.5 $3/$15 per MTok, Opus 5.5 $15/$75, Haiku 5.5 $0.80/$4
- [ ] Calculate `estimated_cost_usd` for each message, store in new column
- [ ] Display token counts in ChatView (small text under each assistant bubble)

**Acceptance:**
- Send 1 message, verify DB has `tokens_in`, `tokens_out`, `estimated_cost_usd`
- ChatView shows "1,234 tokens · $0.018" under assistant message

---

#### Slice 1.11: Ledger Data Model + Response Parsing

**Deliverable:** Extract goals/tasks/questions/decisions from assistant responses.

**Tasks:**
- [ ] Create tables: `goals`, `tasks`, `questions`, `decisions` (see product brief schema)
- [ ] Write `response_parser.py`: regex/heuristics to extract "Goal:", "Task:", "Question:", "Decision:" from assistant text
- [ ] Update system prompt: instruct assistant to use these markers
- [ ] Unit tests: parse example responses, verify items extracted correctly

**Acceptance:**
- Parse "Goal: Plan a trip to Japan" → creates goal item
- Parse "Question: When are you traveling?" → creates question item
- Unit tests pass

---

#### Slice 1.12: Ledger API Endpoints

**Deliverable:** CRUD endpoints for ledger items, auto-create after each chat turn.

**Tasks:**
- [ ] Endpoints: `GET/POST/PATCH /assistants/{id}/goals`
- [ ] Endpoints: `GET/POST/PATCH /assistants/{id}/tasks`
- [ ] Endpoints: `GET/POST/PATCH /assistants/{id}/questions` (answer endpoint)
- [ ] Endpoints: `GET/POST /assistants/{id}/decisions`
- [ ] Hook: after each chat turn, call `response_parser`, create ledger items via API

**Acceptance:**
- Send chat message "Goal: Learn Python", verify `GET /assistants/123/goals` returns 1 goal
- `PATCH /assistants/123/questions/456` with answer updates question status

---

#### Slice 1.13: Side Rail UI (Goals/Tasks/Questions/Decisions)

**Deliverable:** Collapsible side rail showing ledger items.

**Tasks:**
- [ ] Create `SideRail.svelte`: left sidebar container, collapsible sections
- [ ] Create `GoalsPanel.svelte`: list goals, Edit button, mark complete
- [ ] Create `TasksPanel.svelte`: list tasks, checkboxes, mark done
- [ ] Create `QuestionsPanel.svelte`: list questions, Answer button, show answers
- [ ] Create `DecisionsPanel.svelte`: list decisions, view rationale
- [ ] Fetch ledger items on ChatView mount, refresh after each chat turn
- [ ] Wire Edit/Answer/Mark-done buttons to API

**Acceptance:**
- Chat turn creates goal, appears in side rail Goals section
- Click "Edit" on goal, change text, saves to DB
- Click "Answer" on question, type answer, question marked answered
- Collapse/expand sections with ▸/▼ toggle

---

#### Slice 1.14: Polish + Phase 1 QA

**Deliverable:** Bug-free, daily-usable app with Intent Ledger.

**Tasks:**
- [ ] Error handling: show user-friendly errors for Bedrock failures
- [ ] Loading states: spinner while waiting for assistant response
- [ ] Empty states: "No assistants yet" on first launch
- [ ] Delete assistant: "Delete" button in settings (confirm dialog)
- [ ] Keyboard shortcuts: Cmd+Enter to send, Cmd+N for new assistant
- [ ] Dark mode support (reuse existing Tailwind theme)
- [ ] Manual QA: send 20 messages across 3 assistants, verify no crashes
- [ ] Manual QA: verify ledger items persist across restarts
- [ ] Manual QA: edit goal/answer question, verify updates work

**Acceptance:**
- [ ] All Slices 1.1–1.13 features work together
- [ ] Owner uses app daily for 1 week, reports no blockers
- [ ] Python sidecar restarts gracefully on crash
- [ ] Ledger items (goals/tasks/questions/decisions) all functional

**Gate passed → Phase 2**

---

## Phase 2: Spending Dashboard + CSV Reconciliation

**Goal:** Aggregate spending view, CSV export for AWS bill reconciliation.

**Note:** Per-call token tracking moved to Phase 1 (Slice 1.10).

**Gate to Phase 3:** Reconcile with AWS Cost Explorer for 1 month within 5% accuracy.

### Slices

#### Slice 2.1: Assistant Spend Summary View

**Deliverable:** UI showing total spend per assistant.

**Tasks:**
- [ ] New endpoint: `GET /assistants/{id}/spending` (sum tokens, sum cost, group by model)
- [ ] New UI component: `SpendingSummary.svelte` (cards showing total cost, token counts)
- [ ] Add "Spending" tab in ChatHeader or sidebar
- [ ] Show all-time, this-month, this-week filters

**Acceptance:**
- Send 10 messages to "General", 5 to "Code Helper"
- Open Spending view, see "General: $0.45, Code Helper: $0.22" (example values)
- Filter to "This week" shows only recent messages

---

#### Slice 2.2: CSV Export for Reconciliation

**Deliverable:** Download CSV of spending data.

**Tasks:**
- [ ] Endpoint: `GET /spending/export` (query params: start_date, end_date)
- [ ] Return CSV: columns = assistant_name, model, date, tokens_in, tokens_out, estimated_cost
- [ ] UI: "Export CSV" button in Spending view, date range picker
- [ ] Open AWS Cost Explorer instructions modal: "Filter by bedrock-runtime service, compare totals"

**Acceptance:**
- Export CSV for October 2026, open in Excel, sum estimated_cost column
- Open AWS Cost Explorer, filter to October + bedrock-runtime, compare (should be within 5%)
- Document discrepancy sources (e.g., AWS rounds differently, or pricing changed mid-month)

---

#### Slice 2.3: Phase 2 QA + Reconciliation Test

**Deliverable:** 1 month of real spending tracked and reconciled.

**Tasks:**
- [ ] Use app for real work (daily) for 1 month
- [ ] Export CSV at end of month
- [ ] Reconcile with AWS bill
- [ ] Fix any discovered bugs (e.g., missing token counts, wrong pricing)

**Acceptance:**
- [ ] Estimated cost within 5% of actual AWS bill
- [ ] No missing messages in spending table

**Gate passed → Phase 3**

---

## Phase 3: Knowledge Integration

**Goal:** Per-assistant document indexing (trusty-search) and fact memory (trusty-memory).

> Note (2026-11-03): superseded in part. Per the owner definition at the top of this document ("Assistant: definition and knowledge store"), Assistant knowledge uses the trusty-okf store (SPEC-OKF-01), not trusty-search or trusty-memory, and is blocked until that spec is accepted. Slices 3.1-3.7 are not to be built as written.

**Gate to Phase 4:** 2+ weeks stable use with knowledge features, no major bugs.

### Slices

#### Slice 3.1: trusty-search Integration — Upload API

**Deliverable:** API endpoint to upload documents and index them.

**Tasks:**
- [ ] Start trusty-search daemon (reuse `scripts/sandbox_search_daemon.sh`)
- [ ] Python client: `search_client.py` (HTTP to trusty-search API)
- [ ] Endpoint: `POST /assistants/{id}/documents` (multipart/form-data file upload)
- [ ] Store uploaded file in `assistants-app/data/docs/{assistant_id}/{file_id}.pdf`
- [ ] Call trusty-search: `POST /index` with file path, assistant_id as namespace
- [ ] Return `document_id`

**Acceptance:**
- Upload `example.pdf` to "General" assistant
- Verify file in `data/docs/general-123/doc-456.pdf`
- trusty-search index contains chunks from `example.pdf`

---

#### Slice 3.2: trusty-search Integration — Search Command

**Deliverable:** Chat command `/search <query>` retrieves relevant doc excerpts.

**Tasks:**
- [ ] Detect `/search` prefix in user message
- [ ] Call trusty-search: `POST /search` with query + namespace=assistant_id
- [ ] Return top 3 chunks as assistant message (formatted: "From {filename}: {excerpt}")
- [ ] Don't send to Bedrock (local search only)

**Acceptance:**
- Upload PDF with "Q4 revenue was $10M"
- Send `/search revenue`, see assistant reply with excerpt from PDF

---

#### Slice 3.3: trusty-search Integration — Auto-Inject Context

**Deliverable:** Automatically inject relevant docs into Bedrock context.

**Tasks:**
- [ ] On every user message (non-command), run background search: top 3 chunks for query=user message
- [ ] If chunks found, inject into system prompt: "Relevant documents:\n{chunks}\n\nUser question: {message}"
- [ ] Send combined prompt to Bedrock
- [ ] Show "Retrieved 3 documents" indicator in UI

**Acceptance:**
- Upload PDF, send "What was Q4 revenue?" (no `/search` prefix)
- Assistant answers "$10M" based on retrieved context
- UI shows "Retrieved 3 documents" badge

---

#### Slice 3.4: trusty-memory Integration — Fact Extraction

**Deliverable:** Extract facts from conversation and store in trusty-memory.

**Tasks:**
- [ ] Start trusty-memory daemon (reuse `scripts/sandbox_daemon.sh`)
- [ ] Python client: `memory_client.py` (Unix socket to trusty-memory)
- [ ] After every assistant response, call memory: `POST /remember` with namespace=assistant_id, content=assistant message
- [ ] trusty-memory extracts entities and stores facts

**Acceptance:**
- Send "My name is Alice", assistant replies "Nice to meet you, Alice"
- Check trusty-memory: `POST /recall` with query="user name" returns "Alice"

---

#### Slice 3.5: trusty-memory Integration — Auto-Recall

**Deliverable:** Inject recalled facts into Bedrock context.

**Tasks:**
- [ ] On every user message, call memory: `POST /recall` with query=user message, namespace=assistant_id
- [ ] If facts found, inject into system prompt: "Stored facts:\n{facts}\n\nUser question: {message}"
- [ ] Send combined prompt to Bedrock

**Acceptance:**
- Previous conversation: "My name is Alice"
- New conversation: "What's my name?"
- Assistant replies "Your name is Alice" (without repeating the earlier message)

---

#### Slice 3.6: UI for Document Management

**Deliverable:** View uploaded documents, delete documents.

**Tasks:**
- [ ] New UI component: `DocumentsPanel.svelte` (list of uploaded files)
- [ ] Show filename, upload date, file size
- [ ] Delete button (calls `DELETE /assistants/{id}/documents/{doc_id}`, removes from trusty-search index)
- [ ] Drag-drop zone for file upload

**Acceptance:**
- Drag 3 PDFs into chat, see them listed in Documents panel
- Delete 1, verify it's removed from index
- Search for content from deleted doc, no results

---

#### Slice 3.7: Phase 3 QA + Knowledge Validation

**Deliverable:** Knowledge features work reliably.

**Tasks:**
- [ ] Upload 10 PDFs to 1 assistant, verify all indexed
- [ ] Test search across all 10 docs
- [ ] Test memory recall across 5 conversations
- [ ] Verify knowledge isolated per assistant (upload same PDF to 2 assistants, each retrieves only its own copy)
- [ ] Manual QA: use for 2 weeks, report bugs

**Acceptance:**
- [ ] All knowledge features work
- [ ] No cross-assistant knowledge leakage
- [ ] 2+ weeks stable use

**Gate passed → Phase 4**

---

## Phase 4: Rust Port

**Goal:** Port Python prototype to Rust for performance and maintainability.

**Gate to enter:** Phase 1–3 behaviours stable for 2+ weeks, no major UX changes, validated by daily use.

### Slices

#### Slice 4.1: Rust Data Model + SQLite

**Deliverable:** Rust equivalent of `db.py`.

**Tasks:**
- [ ] Create `crates/trusty-assistants` (new crate)
- [ ] Use `rusqlite` for SQLite
- [ ] Define structs: `Assistant`, `Message`, `Spending`
- [ ] Implement CRUD functions
- [ ] Port schema.sql to Rust migrations (or keep as SQL, run via `rusqlite::Connection::execute_batch`)

**Acceptance:**
- Rust tests pass for CRUD operations
- DB schema identical to Python version

---

#### Slice 4.2: Rust Bedrock Client

**Deliverable:** Rust equivalent of `bedrock.converse_stream`.

**Tasks:**
- [ ] Use existing `trusty-common::chat::BedrockProvider` (already exists!)
- [ ] Reuse `aws-sdk-bedrockruntime` from trusty-agents dependencies
- [ ] Test: call Bedrock, stream tokens, verify usage metadata

**Acceptance:**
- Rust Bedrock client streams response identical to Python version

---

#### Slice 4.3: Rust HTTP Server

**Deliverable:** Axum server with same API as Python FastAPI.

**Tasks:**
- [ ] Use `axum` (already in trusty-agents dependencies)
- [ ] Port endpoints: `/assistants`, `/assistants/{id}/chat`, `/assistants/{id}/messages`
- [ ] Port spending endpoints
- [ ] Port document/search endpoints
- [ ] Integration tests: same curl commands work

**Acceptance:**
- `cargo run --bin trusty-assistants` starts server
- Python UI connects and works identically

---

#### Slice 4.4: Tauri Sidecar (Rust Binary)

**Deliverable:** Tauri spawns Rust binary instead of Python.

**Tasks:**
- [ ] Update `tauri.conf.json`: sidecar = `trusty-assistants` binary
- [ ] Ensure Rust binary builds for macOS (aarch64-apple-darwin)
- [ ] Handle sidecar crash same as Python version

**Acceptance:**
- Tauri app launches, spawns Rust binary, UI works

---

#### Slice 4.5: Performance Validation

**Deliverable:** Rust port meets performance goals.

**Tasks:**
- [ ] Measure startup time: target <2s (vs Python ~5s)
- [ ] Measure memory footprint: target <100MB (vs Python ~200MB)
- [ ] Measure latency: first token from Bedrock target <500ms (same as Python)
- [ ] If goals not met, profile and optimize

**Acceptance:**
- [ ] Startup <2s
- [ ] Memory <100MB
- [ ] Latency <500ms

---

#### Slice 4.6: Phase 4 QA + Migration

**Deliverable:** Rust port ships as default.

**Tasks:**
- [ ] Export Python DB, import into Rust (migration script)
- [ ] Verify all assistants, messages, spending data intact
- [ ] Manual QA: use Rust version for 1 week
- [ ] Deprecate Python version (archive in `assistants-app/python-prototype/`)

**Acceptance:**
- [ ] All Phase 1–3 features work in Rust
- [ ] No data loss during migration
- [ ] Owner switches to Rust version permanently

---

## Dependencies Between Phases

```
Phase 1 (Slices 1.1–1.10)
    ↓
Phase 2 (Slices 2.1–2.5)  — depends on Phase 1 (working chat)
    ↓
Phase 3 (Slices 3.1–3.7)  — depends on Phase 1 (working chat) + Phase 2 (cost tracking for knowledge ops)
    ↓
Phase 4 (Slices 4.1–4.6)  — depends on Phases 1–3 (stable feature set)
```

**No slice dependencies within a phase** (all slices in Phase 1 are sequential but can be reordered slightly; Slices 1.1–1.4 are strictly sequential, 1.5–1.10 can overlap).

---

## Estimated Timeline

**Assumptions:**
- 1 engineer (tm-agents PM + delegated builders)
- Phase 1: 2.5–3 weeks (14 slices: core chat + Intent Ledger + spend counter)
- Phase 2: 3–4 days (3 slices: aggregate view + CSV export, per-call tracking already done)
- Phase 3: 2 weeks (7 slices, daemon integration complex)
- Phase 4: 2 weeks (6 slices, port + optimization)

**Total:** ~7.5 weeks from start to Rust port shipped.

**Critical path:** Phase 1 gate (1 week daily use) + Phase 2 gate (1 month reconciliation) + Phase 3 gate (2 weeks stable) = ~6 weeks of validation overlapping with development.

**Realistic timeline:** 10–12 weeks to Phase 4 shipped (accounting for validation gates and iterations).

**Phase 1 breakdown (14 slices):**
- Slices 1.1–1.9: Core chat (assistants, chat view, Tauri shell, settings, tray)
- Slice 1.10: Per-call spend counter (tokens + cost per message)
- Slices 1.11–1.13: Intent Ledger (data model, parsing, side rail UI)
- Slice 1.14: Polish + QA

---

## Risks

### Risk 1: AWS Bedrock throttling during development

**Likelihood:** Medium  
**Impact:** High (blocks all chat features)

**Mitigation:**
- Request quota increase for `bedrock-runtime` if needed
- Implement exponential backoff in boto3 config (already planned)
- Monitor CloudWatch for throttle events

### Risk 2: Python ↔ Tauri IPC flakiness

**Likelihood:** Low  
**Impact:** Medium (app crashes)

**Mitigation:**
- Reuse existing `tagent --api` sidecar pattern (proven)
- Add health check endpoint (`GET /health`) and auto-restart
- If persistent issues, pivot to Electron

### Risk 3: trusty-search/memory daemon integration complex

**Likelihood:** Medium  
**Impact:** Medium (Phase 3 delayed)

**Mitigation:**
- Phase 3 is separate from Phase 1–2 (can defer if needed)
- Python bindings might require new wrapper code
- Consider HTTP API instead of Unix sockets if simpler

### Risk 4: Rust port takes longer than 2 weeks

**Likelihood:** High  
**Impact:** Low (Python version is functional)

**Mitigation:**
- Python version remains usable during port
- Phase 4 is optional (ship Python if Rust port ROI unclear)

---

## Success Metrics

**Phase 1:**
- Owner uses app daily for 1 week
- No crashes

**Phase 2:**
- CSV reconciliation within 5% of AWS bill

**Phase 3:**
- 10 PDFs indexed and searchable
- Memory recalls facts from prior conversations

**Phase 4:**
- Startup <2s, memory <100MB
- All features work identically to Python

---

## Next Actions

1. **Owner approval** of this epic structure
2. **Create GitHub issues** for each slice (linked to this epic)
3. **Start Slice 1.1** (Python env + Bedrock smoke test)
