Added

- **Quarterly re-affirmation for Tier S standing rules, and a `trusty-memory
  doctor` check that reports overdue ones (issue #4890, ADR-0028 D8 point 4).**
  The write-time cap of 20 landed in #4895 and stops the always-injected surface
  from *growing*; it does nothing about a rule that was true when written and
  quietly stopped being true. Such a rule keeps its slot forever and is
  re-transmitted on every turn of every agent session.
  - Every Tier S fact now carries `affirmed_at`, surfaced on both read paths:
    the `list_prompt_facts` MCP tool and `GET /api/v1/kg/prompt-facts`. The
    field is additive — pre-#4890 clients decode both responses unchanged.
  - `affirmed_at` is **derived** from the active KG row's `valid_from` rather
    than stored as a second column. `assert` already rewrites `valid_from` on
    every assertion, so the value is correct by construction on all 93 existing
    palaces with no migration, and no write path can forget to set it.
    Re-asserting a rule **verbatim counts as re-affirmation** — that is the
    deliberate choice, since retyping a rule is exactly the human review the ADR
    asks for.
  - `trusty-memory doctor` gains a fifth check, "Tier S re-affirmation". It
    names every rule unaffirmed for more than 90 days, its age in days, and both
    the re-affirmation path (`kg_assert`) and the retirement path
    (`remove_prompt_fact` with the row's `subject` and `predicate`) — the same
    way the cap's refusal message names the current 20.
  - **The check never retires anything, and never returns `Fail`.** Promotion
    and retirement of a standing rule are deliberate human acts (D8 point 3); a
    `Fail` would flip `doctor`'s exit code and let a stale rule break a scripted
    health gate, pressuring someone into deleting it unreviewed. A stale rule is
    unreviewed, not broken, so the strongest verdict is `Warn`. When the daemon
    is unreachable the check reports `Unknown`, never `Pass`.
