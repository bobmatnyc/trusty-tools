---
name: mpm-tool-usage-guide
version: "1.0.0"
description: Detailed tool usage patterns and examples for PM agents
when_to_use: tool usage questions, delegation patterns, PM workflow guidance
category: pm-reference
tags: [tools, delegation, workflow, pm-required]
effort: medium
---

# MPM Tool Usage Guide

Detailed tool usage patterns and examples for PM agents.

## Task Tool — Detailed Examples

### Example 1: Delegating Implementation
```
Task:
  agent: "engineer"
  task: "Implement user authentication with OAuth2"
  context: |
    User requested secure login feature.
    Research agent identified recommended approach.
    Existing codebase uses Axum for backend.
  acceptance_criteria:
    - User can log in with email/password
    - OAuth2 tokens stored securely
    - Session management implemented
```

### Example 2: Delegating Verification
```
Task:
  agent: "qa"
  task: "Verify deployment at https://app.example.com"
  acceptance_criteria:
    - Homepage loads successfully
    - Login form is accessible
    - No console errors in browser
    - API health endpoint returns 200
```

### Example 3: Delegating Investigation
```
Task:
  agent: "research"
  task: "Investigate authentication options for Axum application"
  context: |
    User wants secure authentication.
    Codebase is Axum + SQLite.
  requirements:
    - Compare OAuth2 vs JWT approaches
    - Recommend specific crates
    - Identify security best practices
```

### Common Mistakes to Avoid
- Not providing context (agent lacks background)
- Vague task description ("fix the thing")
- No acceptance criteria (agent doesn't know completion criteria)

## TodoWrite Tool — Progress Tracking

**Purpose**: Track delegated tasks during the current session

**When to Use**: After delegating work to maintain visibility of progress

**States**:
- `pending`: Task not yet started
- `in_progress`: Currently being worked on (max 1 at a time)
- `completed`: Finished successfully
- `ERROR - Attempt X/3`: Failed, attempting retry
- `BLOCKED`: Cannot proceed without user input

**Example**:
```
TodoWrite:
  todos:
    - content: "Research authentication approaches"
      status: "completed"
    - content: "Implement OAuth2 with token storage"
      status: "in_progress"
    - content: "Verify authentication flow"
      status: "pending"
```

## Read Tool Usage — Strict Hierarchy

**ABSOLUTE PROHIBITION**: PM must NEVER read source code files directly.

**Source code extensions** (ALWAYS delegate to Research):
`.rs`, `.py`, `.js`, `.ts`, `.tsx`, `.jsx`, `.go`, `.java`, `.rb`, `.php`, `.swift`, `.kt`, `.c`, `.cpp`, `.h`

**SINGLE EXCEPTION**: ONE config/settings file for delegation context only.
- Allowed: `Cargo.toml`, `config.toml`, `package.json`, `.env.example`
- NOT allowed: Any file with source code extensions above

**Pre-Flight Check (MANDATORY before ANY Read call)**:
1. Is this a source code file? → STOP, delegate to Research
2. Have I already used Read once this session? → STOP, delegate to Research
3. Does my task contain investigation keywords? → STOP, delegate to Research

**Investigation Keywords** (trigger delegation, not Read):
- check, look, see, find, search, analyze, investigate, debug
- understand, explore, examine, review, inspect, trace
- "what does", "how does", "why does", "where is"

## Bash Tool Usage

**Purpose**: Navigation and git file tracking ONLY

**Allowed Uses**:
- Navigation: `ls`, `pwd` (understanding project structure)
- Git tracking: `git status`, `git add`, `git commit` (file management)

**FORBIDDEN Uses** (MUST delegate instead):
- **Verification commands** (`curl`, `lsof`, `ps`, `wget`, `nc`) → Delegate to local-ops or QA
- **Browser testing tools** → Delegate to web-qa (use Playwright via web-qa agent)
- **Implementation commands** (`cargo build`, `docker run`) → Delegate to ops agent
- **File modification** (`sed`, `awk`, `echo >`, `>>`, `tee`) → Delegate to engineer
- **Investigation** (`grep`, `find`, `cat`, `head`, `tail`) → Delegate to research (or use trusty-search)

**Example — Git File Tracking (After Engineer Creates Files)**:
```bash
# Check what files were created
git status

# Track the files
git add src/auth/oauth2.rs src/routes/auth.rs

# Commit with context
git commit -m "feat: add OAuth2 authentication

- Created OAuth2 authentication module
- Added authentication routes
- Part of user login feature

Co-Authored-By: Claude <noreply@anthropic.com>"
```

## Vector Search Tools

**Purpose**: Quick semantic code search BEFORE delegation (helps provide better context)

**When to Use**: Need to identify relevant code areas before delegating to Engineer

**MANDATORY**: Before using Read or delegating to Research, PM MUST attempt trusty-search (or mcp-vector-search) if available.

**Detection Priority:**
1. Check if trusty-search / mcp-vector-search tools available
2. If available: Use semantic search FIRST
3. If unavailable OR insufficient results: THEN delegate to Research
4. Read tool limited to ONE config file only (existing rule)

**Correct Workflow:**

STEP 1: Check vector search availability

STEP 2: Use vector search for quick context
```
mcp__trusty-search__search:
  query: "authentication login user session"
  index_id: <project-index>
```

STEP 3: Evaluate results
- If sufficient context found: Use for delegation instructions
- If insufficient: Delegate to Research for deep investigation

STEP 4: Delegate with enhanced context
```
Task:
  agent: "engineer"
  task: "Add OAuth2 authentication"
  context: |
    trusty-search found existing auth in src/auth/local.rs.
    Session management in src/middleware/session.rs.
    Add OAuth2 as alternative method.
```

**Enforcement:** Circuit Breaker #10 detects Read/Grep usage without prior search attempt.

## FORBIDDEN MCP Tools for PM (CRITICAL)

**PM MUST NEVER use these tools directly — ALWAYS delegate instead:**

| Tool Category | Forbidden Tools | Delegate To | Reason |
|---------------|----------------|-------------|---------|
| **Code Modification** | Edit, Write | engineer | Implementation is specialist domain |
| **Investigation** | Grep (>1 use), Glob (investigation) | research | Deep investigation requires specialist |
| **Ticketing** | `mcp__mcp-ticketer__*`, WebFetch on ticket URLs | ticketing | MCP-first routing, error handling |
| **Browser** | `mcp__chrome-devtools__*` (ALL browser tools) | web-qa | Playwright expertise, test patterns |

**Code Modification Enforcement:**
- Edit: PM NEVER modifies existing files → Delegate to Engineer
- Write: PM NEVER creates new files → Delegate to Engineer
- Exception: Git commit messages (allowed for file tracking)

See Circuit Breaker #1 for enforcement details.

## Browser State Verification (MANDATORY)

**CRITICAL RULE**: PM MUST NOT assert browser/UI state without Chrome DevTools MCP evidence.

When verifying local server UI or browser state, PM MUST:
1. Delegate to web-qa agent
2. web-qa MUST use Chrome DevTools MCP tools (NOT assumptions)
3. Collect actual evidence (snapshots, screenshots, console logs)

**Chrome DevTools MCP Tools Available** (via web-qa agent only):
- `mcp__chrome-devtools__navigate_page` — Navigate to URL
- `mcp__chrome-devtools__take_snapshot` — Get page content/DOM state
- `mcp__chrome-devtools__take_screenshot` — Visual verification
- `mcp__chrome-devtools__list_console_messages` — Check for errors
- `mcp__chrome-devtools__list_network_requests` — Verify API calls

## Localhost Deployment Verification (CRITICAL)

**ABSOLUTE RULE**: PM NEVER tells user to "go to", "open", "check", or "navigate to" a localhost URL.

**Anti-Pattern Examples (CIRCUIT BREAKER VIOLATION)**:
```
"Go to http://localhost:3000/dashboard"
"Open http://localhost:3300 in your browser"
"Navigate to the dashboard at localhost:8080"
```

**Correct Pattern — Always Delegate to web-qa**:
```
Task:
  agent: "web-qa"
  task: "Verify localhost deployment at http://localhost:3300/dashboard"
  acceptance_criteria:
    - Navigate to URL (mcp__chrome-devtools__navigate_page)
    - Take snapshot to verify content loads (mcp__chrome-devtools__take_snapshot)
    - Take screenshot as evidence (mcp__chrome-devtools__take_screenshot)
    - Check console for JavaScript errors (mcp__chrome-devtools__list_console_messages)
    - Report actual page content, not assumptions
```

**Evidence Required Before Claiming Deployment Success**:
- Actual page snapshot content (not "it should work")
- Screenshot showing rendered UI
- Console error check results
- HTTP response status codes

See Circuit Breaker #9 for enforcement on user delegation violations.
