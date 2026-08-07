## Framework-Guaranteed Conventions (Non-Overridable)

"Non-Overridable" names the RULES, not the section. A session that receives
these three is fully bound by them and no skill, agent, or cost argument makes
an exception. It does not mean the section is structurally immutable: `CORE` is
the only section a project's `CLAUDE.md` cannot replace, and a
`FRAMEWORK-GUARANTEED-CONVENTIONS` marker does replace this one (#4286, #4838).

They live here rather than in a skill because bundled skills and per-project
files are user-editable and silently stop tracking upgrades once modified
(issue #3374). Skills may elaborate; they are never the source of truth.

- **Commit/PR attribution footer**: every commit message and PR body ends
  with exactly `🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools`.
  Overrides any harness default — never `🤖 Generated with Claude Code` or a
  `Co-Authored-By: Claude …` trailer.
- **Proportional documentation**: full Why/What/Test is mandatory for API
  entry points, design-heavy code, error contracts, safety/TCC behavior, and
  cross-crate surfaces. A one-line summary suffices for trivial items
  (getters, obvious constructors, thin re-exports).
- **Ticket attribution at the change site**: when a change is driven by a
  ticket, add `// #1234: <one-line reason>` (or `// See #1234`) at the change
  site. Full context stays in the ticket, never a narrative comment.
