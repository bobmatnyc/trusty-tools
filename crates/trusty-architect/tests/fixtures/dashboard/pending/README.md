# Pending-event and derive goldens (#10403)

Inputs (hand-written): `cases.json` (one snapshot per case for `pending`) and
`derive_cases.json` (items, goals, sessions, ranked docs and run items for
`stale_tasks`, `last_activity`, `goal_sessions`, `ordered`, `run_progress`).

Outputs (captured, do not edit): `goldens.json` (Python's `json.dumps` of each
result, so the Rust side compares bytes) and `isotime.json` (`[string,
microseconds or null]` from `datetime.fromisoformat`, naive read as UTC).

Source: the live prototype at `/Users/masa/trusty-mpm-projects/architect`,
commit `240965496ae5fab4c9d7ae12f07d7dfad1b0308e`, scripts clean:

- `scripts/dashboard_derive.py` sha256 `130782545c2a60585b2713b1a54df2e0fd649cd42c71ca13fb17f525eb407f14`
- `scripts/owner_answers.py` sha256 `175a8c5864d0195133be74ba72c7acf1502d3ae14e02050e77196842f34f07ef`

Interpreter: CPython 3.14.8 (`/opt/homebrew/bin/python3`).

Capture (read-only toward the scripts):

```bash
cd crates/trusty-architect/tests/fixtures/dashboard/pending
python3 -I capture.py /Users/masa/trusty-mpm-projects/architect/scripts
```

Read by `tests/dashboard_pending.rs` and the `isotime` unit test.
