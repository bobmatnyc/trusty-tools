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

## A fifth place to search: the 1.3.8 backlog reset pool

On 2026-08-14 a backlog reset closed 277 issues on age rather than on
verification. Each carries `stateReason: NOT_PLANNED` and a comment stating
plainly that closure was not a claim the issue was fixed.

```bash
gh issue list -R bobmatnyc/trusty-tools --state closed \
  --search '"1.3.8 backlog reset" in:comments' --limit 400
```

One has already been confirmed still-live and reopened
([#5267](https://github.com/bobmatnyc/trusty-tools/issues/5267)) — the
behavior it described was verifiably absent from main. The pool holds real,
unfixed problems, so a hit there is prior art worth reading, not a dead end.

When a problem looks new, check this pool alongside the four keys above. A
match means someone described it before: reopen that issue rather than filing
a fresh one.

## Once the issue exists: move it with `tm issue transition`

The lifecycle labels (`status:in-progress` → `status:coded` → `status:merged` →
`status:tested`) are mutually exclusive, and the repo encodes that as a state
machine in [`issue-state.yaml`](../../issue-state.yaml) at the repo root — the
file `tm issue`'s CWD tier reads, so every `tm issue` verb run from the repo
root uses it with no flag.

```bash
tm issue states                  # the states and the legal edges
tm issue current 1234            # where this issue is now
tm issue transition 1234 status:merged
tm issue transition 1234 closed --note "tm 1.5.16 live run: <what it printed>"
tm issue repair 1234             # an issue that already carries two status: labels
```

`transition` refuses an edge the model does not declare (exit 1, naming the
states you may move to), and performs the add and the remove as ONE
`gh issue edit`, so two `status:` labels can never be observed on one issue. The
`status:tested → closed` edge is declared `requires_note`, which is CLAUDE.md's
"an issue closes only from `status:tested`, with live verification evidence"
made mechanical.

Two limits worth knowing. `tm issue current` and `tm issue transition` both read
the issue through the shared ticket backend, which refuses a CLOSED issue — so
the `closed → open` reopen edge is declared but cannot be driven by `tm` today;
use `gh issue reopen`. And the model is resolved from the process working
directory: run these from the repo root, or pass `--config <path>`.
