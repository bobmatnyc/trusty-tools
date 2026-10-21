## Framework-Guaranteed Conventions (Non-Overridable)

These two bind exactly as the Non-Overridable Rules do, and a
`FRAMEWORK-GUARANTEED-CONVENTIONS` marker likewise replaces the section. Skills
may elaborate; they are never the source of truth (#3374).

- **Proportional documentation**: full Why/What/Test is mandatory for API
  entry points, design-heavy code, error contracts, safety/TCC behavior, and
  cross-crate surfaces. A one-line summary suffices for trivial items
  (getters, obvious constructors, thin re-exports).
- **Ticket attribution at the change site**: a ticket-driven change carries
  `// #1234: <one-line reason>` (or `// See #1234`) there; full context stays
  in the ticket, never a narrative comment.
