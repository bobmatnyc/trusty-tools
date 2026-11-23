# E2E User Story: CTRL → TM → claude-mpm Project Health Check

**Status**: Pending — blocked on #408 (CTRL supervisor loop), #409 (om alias REPL interference), #410 (agent working dir)  
**Type**: Acceptance test / real-world user story  
**Project under test**: ~/Projects/aipowerranking (claude-mpm project)

---

## User Story

> As a user, I have a project `~/Projects/aipowerranking` — a claude-mpm project.
> It has a process to pull articles from the internet daily and index them, which doesn't seem to be working,
> and a process to build a monthly "State of Agentic AI" report, which also doesn't seem to be working.
>
> I want CTRL to:
> 1. Connect to the existing project
> 2. Start an mpm tmux session
> 3. Ask the PM in claude-mpm to report on those two broken processes
> 4. Have TM verify the work is done
> 5. Verify that TM carried out those instructions
> 6. Report back to me with findings

---

## Expected CTRL Flow (post #408–#410 fix)

```
om session run \
  --project ~/Projects/aipowerranking \
  --agent pm \
  --task "Investigate two broken processes: (1) daily article pull + index pipeline, (2) monthly State of Agentic AI build. Report current status, last successful run, what's failing and why, and what needs to happen to get each working again."
```

### Step-by-step CTRL behavior

1. **Connect** — `om connect ~/Projects/aipowerranking` registers project with API server (#409 fix: no REPL interference)
2. **Session** — `om session new --project ~/Projects/aipowerranking --name health-check --agent pm` creates ctrl session
3. **Launch** — CTRL runs `open-mpm --workflow prescriptive --project-dir ~/Projects/aipowerranking --task "..."` (#410 fix: agent CWD = project dir)
4. **PM delegates** — claude-mpm PM in open-mpm delegates to research-agent to investigate both pipelines
5. **TM monitors** — CTRL supervisor reads `workflow-report.md` outcome
6. **Drive to completion** — if `partial`/`fail`, CTRL retries with amended task (up to 3 attempts)
7. **Verify** — CTRL checks that PM actually produced investigation findings (not just a plan)
8. **Escalate if stuck** — if CTRL can't determine pipeline status after 3 attempts, surfaces: "Blocked: could not determine why daily indexer is failing. Logs at: X. How should I proceed?"
9. **Report** — CTRL prints structured summary to stdout and saves to session record

---

## Acceptance Criteria

### AC1: Connection
- [ ] `om connect ~/Projects/aipowerranking` registers project, returns `{id, name, path}` cleanly (no REPL)
- [ ] Project appears in `GET /api/projects`

### AC2: Session lifecycle
- [ ] `om session new` creates session, prints session ID to stdout
- [ ] Session appears in `om session list` with status=idle
- [ ] After run, session transitions to status=completed or status=terminated

### AC3: PM delegation
- [ ] open-mpm PM (in ~/Projects/aipowerranking context) loads claude-mpm project config
- [ ] PM delegates to research-agent to investigate daily indexer
- [ ] PM delegates to research-agent (or same agent) to investigate monthly report builder
- [ ] Both investigations produce findings (not empty)

### AC4: TM verification
- [ ] CTRL reads workflow-report.md and extracts: status, findings per process, next steps
- [ ] CTRL verifies PM produced substantive output (not just "I'll look into it")
- [ ] If PM output is empty/planning-only: CTRL retries with more specific task

### AC5: Report to user
- [ ] Final output includes:
  - Daily indexer: last successful run date, current failure mode, root cause hypothesis
  - Monthly report: last built date, current blocker, what's needed to unblock
- [ ] Report saved to session record in `ctrl-sessions.json`

### AC6: Escalation path
- [ ] If after 3 attempts CTRL cannot determine status: prints specific blocker with paths/logs
- [ ] CTRL does NOT silently fail or report "partial" without explanation

---

## Test Execution (after #408–#410 land)

```bash
# 1. Ensure server running
om start

# 2. Run supervised task
om session run \
  --project ~/Projects/aipowerranking \
  --task "Investigate two broken processes: (1) daily article pull and index pipeline — find last successful run, current failure mode, root cause; (2) monthly State of Agentic AI build — find last built date, current blocker, what's needed to unblock. Produce a written status report for each."

# 3. Expected: structured report printed to stdout
# 4. Verify session record: om session list --project ~/Projects/aipowerranking
```

---

## Known Project Context (~/Projects/aipowerranking)

- **Type**: claude-mpm project
- **Processes**: 
  - Daily: pulls articles from internet, indexes them (cron / scheduler)
  - Monthly: builds "State of Agentic AI" report from indexed articles
- **Symptom**: both processes not working (as of 2026-05-09)
- **Investigation needed**: logs, last run timestamps, cron status, dependency health

---

## Related Issues

- #408 — CTRL supervisor loop (required)
- #409 — om alias REPL interference with session subcommands (required)
- #410 — workflow agents run in --out-dir instead of project dir (required)

---

## Notes

This user story was captured during e2e control plane testing on 2026-05-09. The aipowerranking project has real broken processes that a functioning CTRL should be able to diagnose and report on without the user having to manually run agents.
