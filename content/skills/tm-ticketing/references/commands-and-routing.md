# tm-ticketing — `/tm-ticket` and documentation routing

## `/tm-ticket` Subcommands

High-level orchestration over the ticketing agent, for whichever tracker is
configured:

| Subcommand | Purpose |
|---|---|
| `/tm-ticket organize` | Review, transition states, update priorities, flag stale tickets |
| `/tm-ticket proceed` | Analyze the board, recommend the top 3 next actions |
| `/tm-ticket status` | Health metrics, ticket counts, high-priority work, blockers |
| `/tm-ticket project <url>` | Set the default project/tracker context |

Every subcommand is a PM delegation to the ticketing agent — the PM constructs
the prompt and presents the result, never calling the underlying tools itself.

## Documentation Routing With Ticket Context

With a ticket context present, delegate research findings and specs as ticket
comments (or linked files), and still write a local backup doc under
`docs/research/` (or the configured `documentation.docs_path`). Without ticket
context, everything goes to the local docs path only, named `{topic}-{date}.md`.
