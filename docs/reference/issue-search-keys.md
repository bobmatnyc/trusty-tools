# Rust Issue Search Keys — What To Search By Before Filing

> **Whether a finding earns an issue at all is decided by the Ticket-Promotion
> Gate in the framework skill `tm-ticketing`** (search first, the five promotion
> criteria, the confidence label, outcome-sized granularity, the six-field
> schema). That gate is not restated here. [`CLAUDE.md`](../../CLAUDE.md) names
> the four keys; this page is the rationale behind each one.

A Cargo workspace hands you four high-signal search keys a generic issue search
misses.

| Search key | Example | Why it finds the canonical issue |
|---|---|---|
| **Test name** | `execute_doctor_against_test_daemon` | Rust test names are effectively unique and get quoted verbatim in every prior report and CI log |
| **Panic / error text** | `called Option::unwrap() on a None value`, or a `thiserror` Display string | The literal message is what people paste into issue bodies |
| **Affected symbol** | `WatcherManager::reconcile` | Survives the file moves and module splits that break any path-based search — and this repo splits modules constantly under the 500-SLOC cap |
| **Crate** | `-p trusty-search`, `-p tga` | Scopes to the owning workstream; expand abbreviations first (`CLAUDE.md`'s Abbreviations table — `tm` is trusty-memory, not the binary) |

Search **open and recently closed** issues on all four.

A repeat failure in the same crate is almost always another occurrence of an
existing canonical issue: append the run URL, SHA, command, and failure
signature to that issue rather than filing a second one.
