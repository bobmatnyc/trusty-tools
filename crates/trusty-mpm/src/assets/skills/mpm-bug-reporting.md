---
name: mpm-bug-reporting
version: "1.0.0"
description: Bug reporting protocol for PM and agents to file GitHub issues
when_to_use: Framework bugs, agent errors, skill content errors detected
category: pm-workflow
tags: [bug-reporting, github, issues, pm-required]
effort: high
---

# PM Bug Reporting Protocol

## When to Report Bugs

Report bugs when you encounter:
- **PM instruction errors** or missing guidance
- **Agent malfunction** or incorrect behavior
- **Skill content errors** or outdated information
- **Framework crashes** or unexpected behavior
- **Missing or incorrect documentation**
- **Configuration errors** or invalid defaults

## GitHub Repositories

Route bugs to the correct repository:

| Bug Type | Repository | Owner/Repo |
|----------|------------|------------|
| Core trusty-mpm (CLI, startup, config, orchestration) | trusty-tools | bobmatnyc/trusty-tools |
| Agent bugs (wrong behavior, errors, missing capabilities) | trusty-mpm-agents | bobmatnyc/trusty-mpm-agents |
| Skill bugs (wrong info, outdated, missing content) | trusty-tools | bobmatnyc/trusty-tools |

### Bug Routing Decision Tree

- Bug in `tm`/`trusty-mpm` startup/config/delegation → `bobmatnyc/trusty-tools`
- Bug in agent behavior → `bobmatnyc/trusty-mpm-agents`
- Bug in skill content → `bobmatnyc/trusty-tools`
- Bug in trusty-search/trusty-memory/trusty-analyze → `bobmatnyc/trusty-tools`

## Bug Report Template

When creating an issue, include:

### Title
Brief, descriptive title (50 chars max)
- "PM delegates to non-existent agent"
- "Research skill missing web search examples"

### Labels
Always include:
- `bug` (required)
- `agent-reported` (required)
- Additional context labels:
  - `high-priority` — Critical functionality broken
  - `documentation` — Documentation error
  - `agent-error` — Agent-specific issue
  - `skill-error` — Skill content issue

### Body Structure
```markdown
## What Happened
[Clear description of the bug]

## Expected Behavior
[What should have happened]

## Steps to Reproduce
1. [First step]
2. [Second step]
3. [Third step]

## Context
- Agent: [agent name if applicable]
- Skill: [skill name if applicable]
- Error Message: [full error if available]
- Version: [tm version if known]

## Impact
[How this affects users/workflow]
```

## Using gh CLI

### Prerequisites Check
```bash
gh auth status
```

If not authenticated:
```bash
gh auth login
```

### Creating Issues

**Delegate to ticketing agent** with:
```
Task:
  agent: ticketing
  task: Create GitHub issue for [bug type]
  context: |
    Repository: bobmatnyc/trusty-tools
    Title: [brief title]
    Labels: bug, agent-reported
    Body: |
      ## What Happened
      [description]

      ## Expected Behavior
      [expected]

      ## Steps to Reproduce
      1. [step 1]
      2. [step 2]

      ## Context
      - Agent: [agent name]
      - Error: [error message]

      ## Impact
      [impact description]
```

## Examples

### Core trusty-mpm Bug
```
Task:
  agent: ticketing
  task: Create GitHub issue for core trusty-mpm bug
  context: |
    Repository: bobmatnyc/trusty-tools
    Title: PM fails to load configuration on startup
    Labels: bug, agent-reported, high-priority
    Body: |
      ## What Happened
      PM fails to initialize when config.toml contains invalid syntax.
      No clear error message shown to user.

      ## Expected Behavior
      PM should display clear TOML syntax error with line number and fix suggestion.

      ## Steps to Reproduce
      1. Add invalid TOML to ~/.trusty-mpm/config.toml
      2. Run `tm`
      3. Observe generic error without details

      ## Context
      - Component: Configuration loader
      - Error: "Failed to load configuration"

      ## Impact
      Users cannot diagnose configuration errors.
```

### Agent Bug
```
Task:
  agent: ticketing
  task: Create GitHub issue for agent bug
  context: |
    Repository: bobmatnyc/trusty-mpm-agents
    Title: Research agent fails to search with special characters
    Labels: bug, agent-reported, agent-error
    Body: |
      ## What Happened
      Research agent throws error when search query contains quotes or special chars.

      ## Expected Behavior
      Search queries should be properly escaped and executed.

      ## Steps to Reproduce
      1. Delegate to research: "Search for 'Rust async'"
      2. Research agent attempts search
      3. Error: "Invalid search query"

      ## Context
      - Agent: research
      - Error: grep command fails with unescaped quotes

      ## Impact
      Cannot search for quoted phrases or technical terms with special characters.
```

## Escalation Path

When ticketing agent is unavailable or gh CLI fails:

1. **Log locally** for manual reporting:
   ```
   echo "[BUG] $(date): [description]" >> ~/.trusty-mpm/logs/bugs.log
   ```

2. **Report to PM** for alternative action

3. **User notification**:
   ```
   "Bug detected: [description]. Logged for manual GitHub issue creation."
   ```

## Success Criteria

Bug reporting successful when:
- Issue created in correct repository
- All required labels applied (`bug`, `agent-reported`)
- Body follows template structure
- Title is clear and concise
- Context includes agent/skill name if applicable
- Issue URL returned for tracking

## PM Enforcement

PM MUST:
- Detect bugs during agent interactions
- Delegate bug reporting to ticketing agent
- NOT attempt to create GitHub issues directly
- Follow escalation path if ticketing unavailable
- Log all bug reports for audit trail
