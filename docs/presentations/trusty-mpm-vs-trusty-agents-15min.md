---
title: "trusty-mpm vs trusty-agents: One Platform, Two Products"
duration: "15 minutes (~13 slides)"
audience: "Bob (presenter); designer (`claude design`) builds deck from this markdown"
created: "2026-07-24"
ds_source: "Foundry v2 (docs/design/UI/design-system/); tokens.css + foundry.css; color scheme: rust (#B7410E) + oxide surfaces"
---

# trusty-mpm vs trusty-agents: One Platform, Two Products

## Slide 1: Title Slide
**Key Message:** One platform serving two distinct missions — learn which tool you need.

**Slide Layout:** Full bleed hero; left: trusty-mpm wordmark (robot UNIT 01 overlay), right: trusty-agents wordmark (robot UNIT 02 overlay)

**Headline:** "trusty-mpm vs trusty-agents: One Platform, Two Harnesses"

**Subline:** "Coding-driven workflow automation vs. personal productivity & event-stream processing"

**Speaker Notes:**
We've built two harnesses into the trusty platform. They share libraries (memory, search, review) but have entirely different missions. Let me show you what each one is for and when to use them.

**DS Build Spec:**
- Layout: `grid | split-pane`
- Hero typeface: `--trusty-display` (Chakra Petch 2xl/700)
- Subheading: `--trusty-font` (IBM Plex Sans, lg/500)
- Color accents: --trusty-accent (#B7410E) for highlights; left/right split using --trusty-sidebar-bg (#2b1c12) on left, --trusty-card-bg on right
- Robot marks: idle state, square eyes (robot UNIT numbering per design-system/icons/README.md)

---

## Slide 2: The Platform Layer — One Substrate, Two Harnesses
**Key Message:** Both products build on a shared foundation: memory palace, code search, quality gates, and the event bus.

**Slide Layout:** Stacked architecture diagram; base layer (platform), two equal columns above (mpm & agents)

**Content:**
```
┌───────────────────────────────────────────────────┐
│  trusty-mpm          │     trusty-agents          │
│  (Coding harness)    │  (Task/event harness)      │
├───────────────────────────────────────────────────┤
│         trusty-platform shared layer              │
│ trusty-memory · trusty-search · trusty-review    │
│         trusty-common · event-bus                 │
└───────────────────────────────────────────────────┘
```

**Bullet Points:**
- Both harnesses read/write to the same trusty-memory (personal knowledge base)
- Both can invoke trusty-search for codebase queries
- Both rely on trusty-review for quality gates
- Both publish/subscribe on the event bus for cross-harness coordination

**Speaker Notes:**
Think of the platform layer as the engine room. Everything plugs into memory, search, and quality gates. That's non-negotiable. The harnesses above are the car bodies — one is a project delivery truck (mpm), the other is a personal assistant van (agents). Same engine.

**DS Build Spec:**
- Container: `.card` with --trusty-surface-raised header
- Diagram box: two `.card` variants side-by-side (--trusty-card-bg), --trusty-border-strong divider between them
- Header strip: --trusty-surface-raised (#efe6d8) with --trusty-border-strong top border
- Icon pair: RobotMark UNIT 01 and UNIT 02, rendering states
- Typeface: labels in --trusty-mono, section titles in --trusty-font lg/600

---

## Slide 3: trusty-mpm — The Coding Harness
**Key Message:** A PM-led multi-session daemon that configures, launches, and orchestrates Claude Code sessions for project work.

**Slide Layout:** Left text (bullets), right: call-out box with feature highlights

**Headline:** What is trusty-mpm?

**Content:**
- **What it does:** Manages N concurrent PM-orchestrated Claude Code sessions in tmux worktrees
- **Its scope:** Project provisioning, code delivery, PR workflows, quality gates, multi-agent delegation
- **Its north star:** "The user has to do NOTHING to manage their harness"
- **Core concept:** Sessions = 1:1 bindings to git repos; workstreams = named session clusters

**Key Features (call-out box):**
- Single-URL installer (one `curl | sh`)
- Auto-provisions GitHub repos into worktrees
- PM system prompts + delegated agent sub-commands
- Pause/resume session durability
- Integrated trusty-review gate on all PRs

**Speaker Notes:**
trusty-mpm is the orchestrator for CODING WORK. You give it a repo URL, and it handles everything: spinning up the session, creating the worktree, starting Claude Code with the right instructions, and supervising the session. It's like having a project manager who sets up your workspace and makes sure all your tools are available.

**DS Build Spec:**
- Layout: 60/40 split; left column prose, right column `.card` highlight
- Main heading: --trusty-display (xl/700, --trusty-accent)
- Bullets: --trusty-font (md/400), labels in --trusty-mono (sm/500)
- Highlight box: .card with --trusty-primary-soft background; inner text --trusty-text-primary; border: --trusty-border-strong (1.5px)
- Icons: ActionIcon glyphs (install, settings, rocket) next to feature names

---

## Slide 4: trusty-mpm — The Demo (How It Feels)
**Key Message:** From "I have a repo" to "Claude is in my session" in three commands.

**Slide Layout:** Terminal mockup (monospace code, inline output); timeline callouts on the right

**Demo Script:**
```bash
# 1. Install (one-liner)
$ curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh

# 2. Launch a session from any repo
$ cd my-project && tm sessions new https://github.com/user/my-project

# 3. Claude Code starts, PM loads the instruction set
[Session tm-abc123 provisioned; Claude Code launching...]
[PM: "You are the engineer for trusty-tools. Focus on test coverage."]
```

**Right-side Timeline Callouts:**
1. **30 seconds:** Daemon starts, checks system
2. **1 minute:** Repo cloned into worktree
3. **30 seconds:** Claude Code launches, PM loads instructions
4. **Result:** Engineer agent ready to code, supervised by PM

**Speaker Notes:**
This is the user experience. One URL to install, one command to start. Behind the scenes: git worktree provisioning, session state durability, PM supervision — all automatic. No config files. No manual tmux setup.

**DS Build Spec:**
- Terminal: .card with --trusty-sidebar-bg background, --trusty-sidebar-text foreground, --trusty-mono font
- Command input: --trusty-accent highlight for prompts (`$`), --trusty-success for output lines
- Timeline: vertical connectors (1.5px --trusty-border-strong), callout circles (--trusty-accent, 12px radius), text --trusty-font sm/500
- Timing labels: --trusty-mono xs/500, --trusty-text-secondary color

---

## Slide 5: trusty-mpm — What It Solves (Use Cases)
**Key Message:** Every coding task that involves coordination, quality gates, and multi-agent delegation.

**Slide Layout:** Three-column card layout; each card a use case

**Use Cases:**

### Card 1: PM-Driven Coding
- **Scenario:** Feature branch from CLI issue
- **Actors:** PM (you) + engineer agents (trusty-code subprocesses)
- **Workflow:** Issue context → PM instruction set → agent iteration → PR gate → merge
- **Outcome:** Tested, reviewed, merged code without human hand-holding

### Card 2: Cross-Project Refactoring
- **Scenario:** Change a shared API across 5 repos
- **Actors:** PM + refactor agent + code reviewer agent
- **Workflow:** One command provisions all 5 worktrees; agents parallel-execute with shared context
- **Outcome:** Coordinated PRs, all green, one-click merge

### Card 3: Session Pause/Resume
- **Scenario:** Multi-day project; you close the laptop
- **Actors:** PM + session durability layer
- **Workflow:** `tm sessions pause` → (next day) → `tm sessions resume`
- **Outcome:** Exact state restored; agent continues from where it left off

**Speaker Notes:**
These are not hypothetical. trusty-mpm shines when you have:
- Complex instructions that need to survive agent hand-offs
- Multiple agents collaborating on the same repo
- Quality gates (review, tests) that can't be skipped
- Sessions that span hours or days

**DS Build Spec:**
- Container: three `.card` layouts in a horizontal row (spacing: --trusty-space-4 between each)
- Card header: --trusty-surface-raised strip with --trusty-border-strong divider (1.5px top)
- Card title: --trusty-font md/700, --trusty-accent color
- Bullets: --trusty-font sm/400, labels --trusty-mono xs/600 (bold uppercase)
- Actor role badges: .badge `.badge-primary` style (--trusty-accent background, --trusty-text-inverse foreground)

---

## Slide 6: trusty-agents — The Task/Event Harness
**Key Message:** A tool-calling orchestrator for personal productivity, event connectors, and knowledge work — not code delivery.

**Slide Layout:** Left text (headline + bullets), right: connector icons grid

**Headline:** What is trusty-agents?

**Content:**
- **What it does:** Runs a PM orchestrator + sub-agents; agents call LLM tools to interact with email, Slack, calendar, Google Workspace
- **Its scope:** Tasks, workstreams, personal productivity, event-stream processing
- **Its north star:** "The focus is TASKS, WORKSTREAMS and EVENTSTREAM PROCESSING"
- **Core loop:** Ask → learn → adapt (with memory persistence in trusty-memory)

**Connector Grid (right side):**
- Gmail (email → task inbox)
- Slack (channels/DMs → notifications, actions)
- Google Workspace (Docs, Sheets, Calendar)
- Telegram (mobile surface, bot commands)
- Personal Assistant + Izzie (custom personas)

**Speaker Notes:**
trusty-agents is for YOUR LIFE, not your code. It's the assistant that reads your email, watches Slack, updates your calendar, learns what you care about, and adapts over time. It lives in a macOS app, on Telegram, and in your shell — wherever you are.

**DS Build Spec:**
- Left column: headline --trusty-display (xl/700, --trusty-accent), bullets --trusty-font (md/400)
- Right grid: 2×3 connector card layout; each `.card` with icon (glyph size 24px, --trusty-accent), label --trusty-mono (xs/600), connector name --trusty-font (sm/500)
- Grid spacing: --trusty-space-4 between cards, --trusty-space-3 padding inside each
- Active connector: highlight border --trusty-success-soft background

---

## Slide 7: trusty-agents — How It Feels (UX)
**Key Message:** Always listening, always learning, always just one message away.

**Slide Layout:** Split screen: left = macOS app mockup, right = Telegram screen; chat transcript overlay

**macOS App (left side):**
- Sidebar (oxide --trusty-sidebar-bg): "Inbox", "Tasks", "Calendars", "Memory"
- Main area (.card background): chat window, assistant name (Izzie), streaming response

**Telegram (right side):**
- Telegram mockup; bot name "Personal Assistant"
- Chat message: "Remind me about the Q3 planning meeting"
- Bot response (streaming): "I'll add that to your calendar and flag it in Slack..."

**Shared Context Callout:**
"Both surfaces feed the same persona. Messages sync. Learning persists."

**Speaker Notes:**
Whether you're on your Mac or your phone, the assistant is the same. You can ask it to find a file, schedule a meeting, or draft an email — and it learns from your patterns. It's like having a personal assistant who knows your style and can guess what you need next.

**DS Build Spec:**
- Left mockup: Tauri window chrome (--trusty-sidebar-bg), topbar --trusty-surface-raised, main content .card
- Right mockup: Telegram-style bubble chat; user message --trusty-accent bubble (right), bot message --trusty-card-bg (left); streaming indicator (animated dots, --trusty-accent color)
- Callout box: .badge style, --trusty-info-soft background, --trusty-text-primary text
- Typography: chat text --trusty-font (sm/400), assistant name --trusty-display (sm/600)

---

## Slide 8: trusty-agents — The Learning Loop
**Key Message:** Observe → Store → Retrieve. Trusty-memory makes the agent smarter every interaction.

**Slide Layout:** Circular flow diagram with three nodes; center callout box

**Flow (3-node cycle):**

### Node 1: ASK
- User message enters the agent
- "Find all PRs from last week where the CI was red"

### Node 2: LEARN
- Agent calls tools (Slack search, GitHub API, trusty-search)
- Results flow into trusty-memory via `memory_remember` tool
- Pattern stored: "User cares about CI health trends"

### Node 3: ADAPT
- Next time user asks something related, agent retrieves context from memory
- Response is faster, more contextual, reflects learned preferences
- Memory is shared across all personas (Izzie, Personal Assistant, CTO Bot)

**Center Callout:**
"Every interaction makes the next one better. Learning persists in trusty-memory."

**Speaker Notes:**
This is the magic of trusty-agents. It's not just a chatbot; it's a learning system. Every time you interact with it, it gets smarter about what you care about. And because the memory is shared, all your personas (Izzie, your CTO assistant, etc.) benefit from the same learning.

**DS Build Spec:**
- Circular diagram: three `.card` nodes arranged 120° apart; each card --trusty-card-bg, --trusty-border-strong boundary (1.5px)
- Flow arrows: 1.5px --trusty-accent curves connecting nodes (SVG or CSS arrows)
- Node titles: --trusty-display (md/600), --trusty-accent color
- Node content: --trusty-font (sm/400), --trusty-text-secondary
- Center callout: .card with --trusty-info-soft background, --trusty-text-primary foreground; border-left: 3px --trusty-info
- Memory icon: trusty-memory logo or RobotMark UNIT 02 (memory subsystem)

---

## Slide 9: THE DIFFERENCE (The Money Slide)
**Key Message:** Side-by-side: what each harness orchestrates, who it serves, and why it matters.

**Slide Layout:** Two-column comparison table (visual cards, not boring text table)

### Left Column: trusty-mpm (Coding Harness)
**Orchestrates:** Coding sessions, git worktrees, project workflows  
**Users:** Engineers, PM leads, teams doing code delivery  
**Domain:** Project work, PRs, tests, review gates  
**Personas:** Engineer agents, QA agents, review bots  
**Memory:** Shared codebase context, project state  
**Entry point:** CLI (`tm sessions new <repo>`)  
**Durability:** Session pause/resume; worktree cleanup on close  

### Right Column: trusty-agents (Task/Event Harness)
**Orchestrates:** Event streams, tool calling, personal workflows  
**Users:** Individual knowledge workers, task schedulers  
**Domain:** Email, calendar, Slack, personal productivity  
**Personas:** Personal Assistant, Izzie, CTO Bot  
**Memory:** User preferences, task patterns, learned insights  
**Entry point:** macOS app + Telegram bot  
**Durability:** Persistent memory palace; cross-session learning  

**Bottom Banner (Shared):**
"Both live on trusty-common, trusty-search, trusty-review, and the event bus."

**Speaker Notes:**
This is the key insight. trusty-mpm is **project-scoped**. trusty-agents is **personal-scoped**. One is a truck that hauls code; the other is an assistant that rides with you. They don't compete — they complement. Your mpm session can delegate small tasks to your agents (schedule a meeting, post a summary), and your agents can kick off a coding session via mpm when needed.

**DS Build Spec:**
- Layout: two `.card` columns side-by-side, equal width, --trusty-card-bg background
- Column header: --trusty-surface-raised strip (1.5px --trusty-border-strong top), title --trusty-display (lg/700)
- Left header accent: background --trusty-accent-soft (#f3d9cb)
- Right header accent: background --trusty-info-soft (#dde9f0)
- Row pairs: alternating --trusty-card-bg and --trusty-surface-hover (very subtle)
- Labels (bold): --trusty-mono (xs/600 uppercase), --trusty-text-muted color
- Values: --trusty-font (md/400), --trusty-text-primary
- Bottom banner: full-width --trusty-surface-raised bar, --trusty-text-secondary text, --trusty-mono (sm/500)
- Divider between columns: 1.5px --trusty-border-strong

---

## Slide 10: Shared Platform — The Substrate
**Key Message:** Trusty-search, trusty-memory, trusty-review, event bus — the infrastructure both harnesses depend on.

**Slide Layout:** Horizontal service row; each as a small `.card` with icon and one-liner

**Services:**

### trusty-search
- **Icon:** Magnifying glass (ActionIcon)
- **What:** Hybrid BM25 + vector + knowledge-graph code search
- **Used by:** Both harnesses for context retrieval, definition lookup, bug hunting
- **Example:** Agent queries "Show all uses of authenticate()" → mpm loads context for code review

### trusty-memory
- **Icon:** Brain or archive (custom)
- **What:** Memory palace — persistent semantic storage + vector embeddings
- **Used by:** Both harnesses to store learnings, notes, patterns
- **Example:** Agent learns "User prefers Slack over email"; next time, posts to Slack instead

### trusty-review
- **Icon:** Checkmark (ActionIcon)
- **What:** Automated code review + quality gates
- **Used by:** mpm exclusively (for PR gates); agents can invoke for ad-hoc analysis
- **Example:** Before merge, trusty-review scores complexity, coverage, style

### event-bus
- **Icon:** Lightning bolt (ActionIcon)
- **What:** IPC broadcast for inter-harness events (session started, PR merged, task complete)
- **Used by:** Both, for coordination and reactive workflows
- **Example:** mpm publishes "PR merged"; agents subscribe and update user's task list

**Speaker Notes:**
These are the libraries we all lean on. They're not unique to one harness — they're shared infrastructure. That's by design. It means a learning from one harness can inform the other, and your unified memory palace holds insights from both domains.

**DS Build Spec:**
- Container: horizontal flex row, --trusty-space-4 spacing, centered
- Each service: `.card` (--trusty-card-bg), 100px height, --trusty-radius
- Icon: 24px, --trusty-accent color, centered
- Title: --trusty-font (sm/600, uppercase --trusty-mono), centered below icon
- On hover: --trusty-surface-hover background
- Row container: --trusty-space-6 padding top/bottom
- Connecting line: above row, 1px --trusty-border, --trusty-accent color (dotted)

---

## Slide 11: trusty-code — One-Slide Nod (Where It's Headed)
**Key Message:** The coding-harness work is moving upstream to a model-agnostic implementation; trusty-mpm is the current reference.

**Slide Layout:** Small centered card; context line below

**Card Content:**
- **trusty-code** (tcode): Harness-independent, model-agnostic implementation of PM + instructions + subagents
- **Status:** Proof of concept; reference implementations built for both Anthropic Claude API and open-source models
- **Relevance:** If you want to swap out Claude for Sonnet 3.5, or run the harness locally with Ollama, trusty-code is the future
- **Today:** Use trusty-mpm. It's stable, tested, and feature-complete.

**Context Line (below card):**
"Trusty-code is the next evolution of the coding-harness model. Trusty-mpm is the production harness today."

**Speaker Notes:**
You might hear about trusty-code. It's the direction we're heading — a truly harness-agnostic, model-agnostic implementation. But that's future work. Right now, trusty-mpm is your tool for coding work. Use it.

**DS Build Spec:**
- Card: --trusty-card-bg, centered, max-width 60% of slide
- Header bar: --trusty-surface-raised strip, --trusty-border-strong divider (1.5px top)
- Title: --trusty-display (md/600, --trusty-text-secondary color — downplayed)
- Body: --trusty-font (sm/400)
- Context line: --trusty-mono (xs/400, italic), --trusty-text-muted, centered below
- No icon; minimal styling (this is a footnote, not a hero)

---

## Slide 12: Roadmap & What's Next
**Key Message:** Both harnesses are shipping. What we're working on in the next quarter.

**Slide Layout:** Timeline (horizontal bars, 3 lanes: mpm, agents, platform)

**Next 3 Months:**

### trusty-mpm Lane
- ✅ Session pause/resume (shipped)
- 🔄 **Workstream labels**: Label every issue/PR with `ws/<session-name>` for cross-session filtering
- 🔄 **Multi-repo refactoring**: Provision 5+ worktrees in one command
- 📅 Trusty-code integration: mpm can delegate to tcode for alternate model/harness

### trusty-agents Lane
- ✅ macOS app (shipping this week)
- 🔄 **Telegram bot**: Full persona support (Personal Assistant, CTO Bot, Izzie)
- 🔄 **Learning loop soak**: 30-day user study on memory retention and adaptation
- 📅 Google Workspace connectors: Calendar scheduling, Doc drafting

### Platform Lane
- ✅ Event bus (loopback-only, launched)
- 🔄 **Trusty-search 0.40.0**: Dark mode, improved UI
- 🔄 **Trusty-memory scalability**: Support 1M+ memories with embedding pruning
- 📅 Operator tooling: `/status` metrics, cross-harness traceability

**Speaker Notes:**
Both products are moving fast. We've got real users. The platform is stabilizing. The big bets this quarter are workstreams (better session organization), telegram (mobile), and multi-repo refactoring. We're hiring for these areas — if you want to ship, let's talk.

**DS Build Spec:**
- Timeline container: three horizontal bars (.card style, stacked vertically, --trusty-space-4 between each)
- Bar header: --trusty-surface-raised, title in --trusty-display (sm/600)
- Lane background: --trusty-surface-hover (very faint)
- Timeline items: checkmark (✅ --trusty-success), circle (🔄 --trusty-warning), or dot (📅 --trusty-info) indicator in --trusty-mono, item text --trusty-font (sm/400)
- Status colors: success = --trusty-success, in-progress = --trusty-warning, planned = --trusty-info-soft background

---

## Slide 13: Demo + Next Steps
**Key Message:** Try it yourself. Install, launch a session, watch it work.

**Slide Layout:** Two-column; left = QR code + URL, right = 3 next-step bullets

### Left: Installation
**Headline:** "Try trusty-mpm"

**QR Code** pointing to:
```
https://github.com/bobmatnyc/trusty-tools#installation
```

**One-liner:**
```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
```

### Right: Next Steps
1. **Run `tm status`** — Check daemon status
2. **Run `tm sessions new https://github.com/<your-repo>`** — Start a session on your project
3. **Join the workstream** — Slack channel #trusty-users; Discord for open-source contributors

**Bottom Banner (call-to-action):**
"Questions? Bob is in the building. Let's ship."

**Speaker Notes:**
If you want to try it, here's everything you need. The install is one line. The demo is three commands. Questions? Let's build this together.

**DS Build Spec:**
- Split layout: 50/50 (left QR/install, right bullets)
- Left side: .card container, --trusty-card-bg, centered; QR code 120px × 120px, --trusty-border-strong frame (1.5px); URL text --trusty-mono (xs/500) below
- Code block: --trusty-sidebar-bg background, --trusty-sidebar-text foreground, --trusty-mono (sm/400), padding --trusty-space-3
- Right side: bullet list, --trusty-font (md/400), numbers in --trusty-mono (xs/600), --trusty-accent highlights
- Bottom banner: --trusty-surface-raised background, --trusty-text-primary text, --trusty-display (lg/600), centered
- Call-to-action: --trusty-accent underline on "Bob is in the building"

---

# Appendix: Timing Breakdown

| Section | Slide | Duration | Notes |
|---------|-------|----------|-------|
| Intro | 1–2 | 1.5 min | Title, platform thesis |
| trusty-mpm narrative | 3–5 | 4 min | What it is, demo, use cases |
| trusty-agents narrative | 6–8 | 4 min | What it is, UX, learning loop |
| The differentiation | 9–10 | 3 min | Money slide + shared substrate |
| Outlook | 11–13 | 2.5 min | trusty-code note, roadmap, demo |
| **Total** | **13 slides** | **~15 min** | Leaves 3–5 min for Q&A |

---

# Design System Reference Summary

**Foundry v2** (as of 2026-07-18; source: `docs/design/UI/design-system/`)

### Color Tokens (Light Theme)
- **Accent (Rust core):** `--trusty-accent: #B7410E`; hover: `#8A2F0B`; soft: `#F3D9CB`
- **Sidebar (oxide chassis):** `--trusty-sidebar-bg: #2B1C12`; text: `#E6D8C8`
- **Content (warm paper):** `--trusty-content-bg: #F5EFE7`; card: `#FFFDF9`
- **Status:** success (#3F6F2A), warning (#B07D10), danger (#C2331F), info (#3D6B8A)
- **Surface:** raised (#EFE6D8), hover (rgba 6% opacity rust)

### Typography
- **Display:** `--trusty-display` = Chakra Petch 500–700, hero/headings
- **Body:** `--trusty-font` = IBM Plex Sans 400–700, prose
- **Mono:** `--trusty-mono` = IBM Plex Mono 400–600, code/labels (uppercase, letterspaced)
- **Scale:** xs (0.75rem), sm (0.875rem), md (1rem), lg (1.125rem), xl (1.5rem), 2xl (2rem)

### Components (from foundry.css)
- `.btn`, `.btn-primary`, `.btn-secondary`, `.btn-ghost`, `.btn-danger`
- `.card`, `.card-header`
- `.badge` (rectangular 4px radius, mono uppercase 10px)
- `.modal`, `.toast` (with `.toast-success`, `.toast-danger`)
- `.stat`, `.stat-card` (for metric tiles)
- `.table`, `.table-hover` (row hover: --trusty-surface-hover)
- `.sidebar`, `.topbar`, `.appshell`

### Layout & Spacing
- **Sidebar width:** 240px
- **Topbar height:** 56px
- **Grid:** 4px base; tokens: --trusty-space-1 (4px) through --trusty-space-6 (32px)
- **Radius:** 3px (sm), 5px (default), 8px (lg)
- **Borders:** 1px (dividers), 1.5px (containers)

### Dark Theme (Night Shift)
Activate with `<html data-theme="dark">` or `.dark` class; accent brightens to #D97742 for contrast.

---

# Notes for Designer ("claude design")

1. **Slide structure:** Each slide follows: title, key message (one line), content bullets/visuals, speaker notes, DS build spec
2. **Color discipline:** Use only tokens listed above; never hardcode hex unless noted as a reference
3. **Robot marks:** UNIT 01 (Search), UNIT 02 (Memory/Agents), UNIT 03 (Analyze)—see `docs/design/UI/design-system/icons/README.md` for glyph specs; keep states: idle, receiving, working
4. **Spacing:** Align to 4px grid; use --trusty-space-N tokens consistently
5. **Typography:** Never mix fonts within a hierarchy; mono is for labels/code only; display is for hero/section headers only
6. **Accessibility:** Every status color paired with text/icon; never color alone; high contrast in both light and dark themes
7. **Dark theme:** Test all slides in both light and dark (Night Shift) modes; token swaps must be automatic via CSS custom properties
8. **Component reuse:** Foundry.css provides .btn, .card, .badge, .modal, .toast—use these as the foundation; avoid custom one-off styles

---

**End of Presentation Outline**

*Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools*
