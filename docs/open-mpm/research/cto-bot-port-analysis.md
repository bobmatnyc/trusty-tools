# CTO Bot Port Analysis — open-mpm Migration Guide

**Date:** 2026-04-30  
**Source:** ~/Duetto/cto  
**Purpose:** Extract everything needed to port CTO bot capabilities to open-mpm

---

## 1. Skills / Knowledge Files

### Project-Specific Skills (`~/.claude/skills/` within the repo)

| Skill Directory | File | Purpose |
|---|---|---|
| `.claude/skills/cto-db/` | `skill.md` | Full SQLite schema for `data/cto.db` — 28 tables, query patterns, gotchas |
| `.claude/skills/cto-apex/` | `SKILL.md` | APEX review workflow — pull inbox, analyze artifacts, submit back to APEX repo |
| `.claude/skills/apex-framework/` | `SKILL.md` | Full APEX framework reference — artifact types, frontmatter schemas, CLI, CI/validation |
| `.claude/skills/fact-finder.md` | (flat file) | `mcp-fact-finder` tool reference — search_facts, get_entity_facts, compare_sources, check_inconsistencies |
| `.claude/skills/gworkspace/` | `SKILL.md` | Google Workspace MCP + Python API — Gmail (21 tools), Calendar (10), Drive (17), Docs (16), Sheets (12), Slides (15), Tasks (10) |
| `.claude/skills/bob-slack-voice/` | `SKILL.md` | Bob Matsuoka Slack voice guide — double-hyphen style, message templates, do/don't list |
| `.claude/skills/duetto-code-intelligence/` | `SKILL.md` | Duetto Code Intelligence platform — RAG search, MCP integration at code-intelligence.dev.duettosystems.com |
| `.claude/skills/duetto-apex-companion.md` | `SKILL.md` | APEX Companion API at apex-companion.dev.duettosystems.com |
| `.claude/skills/duetto-apex-engineer.md` | (flat) | APEX engineer workflow |
| `.claude/skills/duetto-apex-pm.md` | (flat) | APEX PM workflow |

### Key CLAUDE.md Sections Injected as Prompt Context

Located at `~/Duetto/cto/CLAUDE.md`. Contains:
- Full org structure (ELT + SELT names/roles)
- Directory conventions for all 11 project categories
- Database access patterns (SQLite, DuckDB, KuzuMemory, vector search)
- Notion API integration pattern
- Document naming conventions and status lifecycle

---

## 2. MCP Server Configuration

**Primary config location:** `~/Duetto/.mcp.json` (parent directory — inherited by cto project)

No `.mcp.json` exists in `~/Duetto/cto/` itself — the bot reads both the project root and parent directory via `_merge_mcp_configs()` in `config.py`.

### MCP Servers Configured (`~/Duetto/.mcp.json`)

```json
{
  "mcpServers": {
    "kuzu-memory": {
      "type": "stdio",
      "command": "kuzu-memory",
      "args": ["mcp"],
      "env": {
        "KUZU_MEMORY_PROJECT_ROOT": "/Users/masa/Clients/Duetto/CTO",
        "KUZU_MEMORY_DB": "/Users/masa/Clients/Duetto/CTO/kuzu-memories"
      }
    },
    "mcp-vector-search": {
      "type": "stdio",
      "command": "mcp-vector-search",
      "args": ["mcp"],
      "env": {
        "MCP_ENABLE_FILE_WATCHING": "true"
      }
    },
    "granola-notes": {
      "type": "stdio",
      "command": "/opt/homebrew/bin/granola-mcp",
      "args": []
    },
    "gworkspace-mcp": {
      "type": "stdio",
      "command": "gworkspace-mcp",
      "args": ["mcp"]
    },
    "slack-user-proxy": {
      "type": "stdio",
      "command": "slack-user-proxy",
      "args": []
    },
    "duetto-memory": {
      "type": "http",
      "url": "https://mcp-services.dev.duettosystems.com/memory/mcp"
    }
  }
}
```

### Additional MCP Servers Referenced in Bot Code

The bot's `MCPClientService._WANTED` set expects these servers:
- `kuzu-memory` — local KuzuDB graph memory
- `mcp-vector-search` — semantic search over project files
- `granola-notes` — Granola meeting notes
- `confluence-mcp` — Confluence wiki search (loaded from a separate local config not found in tree)
- `mcp-fact-finder` — cross-source fact verification
- `gworkspace-mcp` — Google Workspace (Gmail, Calendar, Drive)
- `duetto-directory` — Okta/directory service (URL not found, likely in `.env.local`)
- `duetto-memory` — remote HTTP memory service (above)

### APEX Repo MCP Config (referenced in apex-framework skill)

```json
{
  "mcpServers": {
    "sfdc-mcp": {
      "url": "https://sfdc-mcp.dev.duettosystems.com/mcp"
    }
  }
}
```
File is gitignored in the APEX repo.

---

## 3. Kuzu Memory

**Database location:** `~/Duetto/cto/.kuzu-memory/memories.db`  
**Config file:** `~/Duetto/cto/.kuzu-memory-config` — contains `mode: subservient`, meaning it is managed by claude-mpm.

**The KuzuDB is a binary database** — not a JSON/YAML export. To extract memories for porting:

```bash
# From ~/Duetto/cto:
kuzu-memory mcp  # then call kuzu_export_shared or kuzu_stats
```

Or use the MCP tool `mcp__kuzu-memory__kuzu_export_shared` to dump memories to a portable format.

The in-bot `KuzuMemoryService` (`app/cto_bot/services/memory.py`) wraps the `kuzu-memory` MCP server tools: `kuzu_recall`, `kuzu_remember`, `kuzu_learn`, `kuzu_stats`.

---

## 4. Tool Definitions (Full JSON Schema)

### 4a. `query_analytics` — DuckDB analytics.duckdb

```json
{
  "name": "query_analytics",
  "description": "Execute a read-only SQL query against the CTO analytics DuckDB database (data/analytics.duckdb). Contains engineer activity (commits, PRs, DORA metrics in fact_weekly_engineer, v_dora_weekly, v_pod_weekly, v_org_weekly), recruiting pipeline + forecast (recruiting_requisitions, recruiting_pipeline, hiring_forecast), budget breakdown (budget_by_initiative, budget_by_product, budget_comparison), and dim tables (dim_engineer, dim_repo, dim_sprint, dim_week). Use for any metrics, forecasting, or analytics question. SELECT/WITH only — no writes. Capped at 500 rows.",
  "parameters": {
    "type": "object",
    "properties": {
      "sql": {
        "type": "string",
        "description": "Read-only SQL query (SELECT or WITH)."
      },
      "description": {
        "type": "string",
        "description": "Short human-readable description of what this query answers."
      }
    },
    "required": ["sql", "description"]
  }
}
```

### 4b. `query_cto_db` — SQLite cto.db

```json
{
  "name": "query_cto_db",
  "description": "Execute a read-only SQL query against the cto.db SQLite database (data/cto.db). Useful tables include rd_budget_2026 (monthly headcount + cost plan with jan_26..dec_26 columns, rationale, level, manager), llm_usage (LLM API call log), llm_usage_stats (aggregated LLM cost), plus person, work_type, product, bus_factor_risks. SELECT/WITH only. Capped at 500 rows.",
  "parameters": {
    "type": "object",
    "properties": {
      "sql": {
        "type": "string",
        "description": "Read-only SQL query (SELECT or WITH)."
      },
      "description": {
        "type": "string",
        "description": "Short human-readable description of what this query answers."
      }
    },
    "required": ["sql", "description"]
  }
}
```

### 4c. `generate_chart` — PNG chart to Slack

```json
{
  "name": "generate_chart",
  "description": "Render a PNG chart and upload it to Slack. Supported chart_type: 'bar', 'line', 'stacked_bar'. Provide labels (x-axis) plus either `values` (single series, bar only) OR `series` (dict of name -> list of numbers, required for line/stacked_bar). After calling this tool, copy the [CHART: ...] token from the tool response VERBATIM into your final reply on its own line so the chart is uploaded to Slack.",
  "parameters": {
    "type": "object",
    "properties": {
      "chart_type": {
        "type": "string",
        "enum": ["bar", "line", "stacked_bar"],
        "description": "Type of chart to generate."
      },
      "title": {"type": "string", "description": "Chart title."},
      "ylabel": {"type": "string", "description": "Y-axis label."},
      "labels": {
        "type": "array",
        "items": {"type": "string"},
        "description": "X-axis labels (e.g. month names, team names)."
      },
      "values": {
        "type": "array",
        "items": {"type": "number"},
        "description": "Single-series numeric values (bar only)."
      },
      "series": {
        "type": "object",
        "description": "Multi-series data: {series_name: [numbers]}. Required for line and stacked_bar.",
        "additionalProperties": {
          "type": "array",
          "items": {"type": "number"}
        }
      }
    },
    "required": ["chart_type", "labels"]
  }
}
```

### 4d. `get_train_schedule` — Metro North

```json
{
  "name": "get_train_schedule",
  "description": "Get upcoming Metro-North train departures between two stations. Use this when the user asks about train times, departures, arrivals, or schedules on Metro-North.",
  "parameters": {
    "type": "object",
    "properties": {
      "from_station": {
        "type": "string",
        "description": "Departure station name, e.g. 'Grand Central Terminal', 'Stamford', 'New Haven'. Partial names accepted."
      },
      "to_station": {
        "type": "string",
        "description": "Destination station name, e.g. 'Stamford', 'New Haven', 'Grand Central'. Partial names accepted."
      },
      "count": {
        "type": "integer",
        "description": "Number of trains to return (default 5, max 20).",
        "default": 5
      }
    },
    "required": ["from_station", "to_station"]
  }
}
```

Data source: `https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/mnr%2Fgtfs-mnr` (public, no API key). Parsed from GTFS-RT protobuf via `google-transit` Python package.

### 4e. `get_train_alerts` — Metro North Alerts

```json
{
  "name": "get_train_alerts",
  "description": "Get active Metro-North service alerts. Use this when the user asks about delays, service disruptions, or alerts on Metro-North.",
  "parameters": {
    "type": "object",
    "properties": {
      "line": {
        "type": "string",
        "description": "Optional line name to filter alerts, e.g. 'New Haven', 'Harlem', 'Hudson'. Omit to get all alerts."
      }
    },
    "required": []
  }
}
```

### 4f. MCP-Proxied Tools (wrapped by MCPClientService)

These are exposed to Bedrock through dedicated service wrappers, but the underlying calls go to MCP servers:

| Tool Name (Bedrock) | MCP Server | MCP Tool |
|---|---|---|
| `search_meeting_notes` | `granola-notes` | `granola_search` / `search_granola_transcripts` |
| `search_codebase` | `mcp-vector-search` | `search_hybrid` |
| `search_confluence_docs` | (local Confluence index) | in-process |
| `get_calendar_events` | `gworkspace-mcp` | `get_events` |
| `check_availability` | `gworkspace-mcp` | `query_free_busy` |
| `search_email` | local Gmail cache | file-based digest |
| `get_gfa_report` | local GFA engine | in-process |
| `get_team_members` | `data/cto.db` | in-process SQLite |
| `get_budget_data` | `data/cto.db` | in-process SQLite |
| `get_product_portfolio` | `data/cto.db` | in-process SQLite |
| `get_bus_factor_risks` | `data/cto.db` | in-process SQLite |

Kuzu memory (`kuzu_recall`, `kuzu_remember`) is called directly by context gathering code, not exposed as a Bedrock tool.

---

## 5. Canvas / Projects Files

The `projects/` directory contains 11 top-level categories:

| Path | Contents |
|---|---|
| `projects/ELT/` | OKRs, cto/, events/, legal/, finance/, executive-summaries/, vendors/ |
| `projects/SELT/` | Charter, all-hands/, meetings/ (weekly SELT reviews, HTML reports) |
| `projects/engineering/` | Metrics, SDLC, automation/, tactical/, code/, analytics/, architecture/ |
| `projects/product/` | Inventory, business-logic/, ontology/, customer/, market-research/, integrations/, features/ |
| `projects/research/` | Strategic research only — strategic/monolith, strategic/strangler, strategic/HotStats |
| `projects/security/` | Security audits, infosec/ |
| `projects/systems/` | JIRA, Confluence, aws/, tools/evaluations/, operations/ |
| `projects/people/` | Team analysis, contractors, offshore, hiring, research-artifacts/ |
| `projects/hotstats/` | HotStats partnership projects |
| `projects/meetings/` | Meeting notes in `2026-W##/` weekly folders |
| `projects/writing/` | Articles, presentations, polished content |

Key standing documents found:
- `projects/ELT/OKR/` — 2026 R&D OKRs (canonical)
- `projects/SELT/` — SELT charter
- `projects/ELT/allocation/ALLOCATION-MODEL.md` — headcount allocation model
- `projects/ELT/cto/100-days.md` — CTO 100-day plan
- `projects/ELT/cto/BOARD-UPDATE-2026-Q1.md` — board update
- `projects/engineering/architecture/polaris/` — Polaris Architecture project
- `projects/research/strategic/strangler/` — Strangler Pattern POC

The Slack bot exposes these via `[CREATE_CANVAS: path]` and `[SAVE_CANVAS: path]...[/SAVE_CANVAS]` tokens in responses, which are intercepted by the handler and turned into Slack Canvas objects.

---

## 6. Config Files — Key Names (No Values)

### `~/Duetto/cto/.env.local.example` — All Required Keys

**Slack:**
- `SLACK_BOT_TOKEN`
- `SLACK_APP_TOKEN`

**Atlassian (JIRA/Confluence):**
- `ATLASSIAN_PAT`

**Google Workspace (OAuth):**
- `GOOGLE_OAUTH_CLIENT_ID`
- `GOOGLE_OAUTH_CLIENT_SECRET`
- `GOOGLE_PROJECT_ID` (= `claude-mpm-485205`)
- `GOOGLE_REDIRECT_URI`

**Notion:**
- `NOTION_API_KEY`

**Datadog:**
- `DD_API_KEY`
- `DD_APP_KEY`
- `DATADOG_API_KEY`
- `DATADOG_APP_KEY`
- `DD_SITE`

**GitHub:**
- `GITHUB_TOKEN`

**Salesforce:**
- `SALESFORCE_USERNAME`
- `SALESFORCE_PASSWORD`
- `SALESFORCE_SECURITY_TOKEN`

**AWS (Bedrock — set by AWS CLI, not in .env):**
- `AWS_REGION` (default: `us-east-1`)
- `BEDROCK_MODEL` (default: `us.anthropic.claude-sonnet-4-6`)

**Bot configuration (set in environment/systemd, not in .env.example):**
- `BOT_ALLOWED_USERS` — format: `ID1:Name1:TIER1,ID2:Name2:TIER2`
- `BOT_MPM_SDK_USERS` — comma-separated Slack IDs
- `BOT_MAX_HISTORY` (default: 40)
- `BOT_COMPRESSION_THRESHOLD` (default: 30)
- `KUZU_MEMORY_DB` — path to kuzu memories.db

**Additional keys found in `.env.keys`:**
- `GAMMA_API_KEY` — Gamma presentation API

### Slack User IDs (hardcoded defaults)
- `U0A6V2W1M2R` — Robert Matsuoka (CTO), tier: ALL
- `U0ALDQLBU79` — Andrea Kovac (Eng Ops), tier: ALL
- `U09331EP3MX` — Alex Zoghlin (CEO), tier: ANALYTICS

---

## 7. VIRTUAL_CTO_PROMPT (Public-Facing Variant)

Full text from `/Users/masa/Duetto/cto/app/cto_bot/prompts.py`:

```
You are the Virtual CTO — a public-facing AI assistant representing Robert Matsuoka,
CTO of Duetto Research, a hospitality revenue management SaaS company.

## What You Can Discuss
- Technology strategy and engineering leadership philosophy
- Software architecture patterns (monolith migration, microservices, strangler pattern)
- AI/ML in hospitality and revenue management
- Engineering team culture, DevOps practices, CI/CD
- Open source, cloud architecture (AWS), Python, Java ecosystems
- General career advice for engineering leaders
- Published articles and public talks by the CTO

## What You MUST NOT Discuss
- Specific employees, contractors, or personnel matters
- Salaries, compensation, budgets, or financial details
- Internal org structure, headcount, or team composition
- Confidential projects, roadmap, or unreleased features
- Customer names, contracts, or business relationships
- Security vulnerabilities or infrastructure details
- Internal tools, databases, or system access

If asked about any restricted topic, politely decline:
"I can't share internal details, but I'm happy to discuss the general technology approach."

## Response Style
- Be helpful, professional, and technically substantive
- Use Slack markdown: *bold*, _italic_, bullet lists
- Keep answers concise but informative
- Represent the CTO's perspective on technology and engineering leadership
- You may reference publicly available information about Duetto

## Important
- You have NO access to internal databases, documents, or tools
- You cannot look up people, meetings, budgets, or projects
- All conversations are logged for security purposes
- You are a read-only, knowledge-based assistant
```

---

## 8. Primary System Prompt (SYSTEM_PROMPT)

Defined in `/Users/masa/Duetto/cto/app/cto_bot/prompts.py`. Key elements:

**Identity:** CTO Assistant for Duetto Research, private to Robert Matsuoka and Andrea Kovac only.

**About Duetto:** Hospitality revenue management SaaS, ~1.2M LOC Java monolith + microservices, ~178 R&D staff (94 FTE + ~84 contractors), ~$25M annual R&D spend.

**Data access claimed in prompt:**
- `cto.db` (SQLite) via `query_cto_db`
- `analytics.duckdb` (DuckDB) via `query_analytics`
- Confluence (1,700+ wiki pages)
- JIRA (856+ issues)
- Git activity (148 repos)
- Granola (live meeting notes via MCP)
- Gmail (recent priority emails)
- Okta (employee/contractor identity)
- Fact Finder (cross-source fact verification)
- Bot changelog (live git log of `app/cto_bot/`)

**Special tokens:** `[CHART: {...}]`, `[CREATE_CANVAS: path]`, `[SAVE_CANVAS: path]...[/SAVE_CANVAS]`, `[MOVE_FILE: old → new]`, `[DELETE_FILE: path]`, `[READ_FILE: path]`

**Special commands:** `!clear`, `!help`, `!status`, `!memory`, `!canvas`, `!bug`, `!ghissue`, `!mpm`, `!virtual-cto`

---

## 9. Architecture Summary for Port

The bot is a **Python Slack bot** (async, Socket Mode via `slack-bolt`) backed by **AWS Bedrock** (Claude Sonnet 4-6 via `converse` API with tool use). It is NOT a Claude Code harness. The open-mpm port would need to:

1. **Adapt the tool registry** — the 16 Bedrock tools become open-mpm TOML agent tool definitions
2. **Adapt the MCP clients** — currently Python subprocess/HTTP; in open-mpm these are already first-class
3. **Adapt the system prompt** — `SYSTEM_PROMPT` becomes the PM agent's `system_prompt.content` in TOML
4. **Adapt the databases** — cto.db (SQLite) and analytics.duckdb are accessed directly; in open-mpm, sub-agents would run Python scripts via subprocess
5. **Adapt the skills** — the `.claude/skills/*.md` files map directly to open-mpm `.open-mpm/skills/*.md`

---

## 10. File Paths Reference

| What | Path |
|---|---|
| Main system prompt | `app/cto_bot/prompts.py` |
| Bot config + MCP loading | `app/cto_bot/config.py` |
| Bedrock tool registry | `app/cto_bot/services/registry.py` |
| All tool definitions | `app/cto_bot/services/*.py` (ServiceSpec classes) |
| Metro North impl | `app/metro_north.py` |
| CTO DB schema skill | `.claude/skills/cto-db/skill.md` |
| APEX workflow skill | `.claude/skills/cto-apex/SKILL.md` |
| APEX framework skill | `.claude/skills/apex-framework/SKILL.md` |
| Google Workspace skill | `.claude/skills/gworkspace/SKILL.md` |
| Fact Finder skill | `.claude/skills/fact-finder.md` |
| Bob Slack voice skill | `.claude/skills/bob-slack-voice/SKILL.md` |
| MCP config (parent) | `~/Duetto/.mcp.json` |
| API keys (example) | `.env.local.example` |
| Kuzu memory DB | `.kuzu-memory/memories.db` |
| Main analytics DB | `data/analytics.duckdb` |
| Main people/budget DB | `data/cto.db` |
| Projects (all docs) | `projects/` (11 top-level categories) |
