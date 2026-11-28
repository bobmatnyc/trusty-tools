# Pending-event and derive goldens (#10403)

Inputs (hand-written): `cases.json` (one snapshot per case for `pending`) and
`derive_cases.json` (items, goals, sessions, ranked docs and run items for
`stale_tasks`, `last_activity`, `goal_sessions`, `ordered`, `run_progress`).

Outputs (captured, do not edit): `goldens.json` (Python's `json.dumps` of each
result, so the Rust side compares bytes) and `isotime.json` (`[string,
microseconds or null]` from `datetime.fromisoformat`, naive read as UTC).

Source: the live prototype at `<architect-checkout>`,
commit `240965496ae5fab4c9d7ae12f07d7dfad1b0308e`, scripts clean:

- `scripts/dashboard_derive.py` sha256 `130782545c2a60585b2713b1a54df2e0fd649cd42c71ca13fb17f525eb407f14`
- `scripts/owner_answers.py` sha256 `175a8c5864d0195133be74ba72c7acf1502d3ae14e02050e77196842f34f07ef`

Last recapture (#10765, answered entries carry `answer_note`): from
`python/trusty-architect/scripts`, `dashboard_derive.py` sha256
`b1f52b0320e5789732d9297270eca403ad3a91f16d484491aea7e57a3d2c113d`.

Interpreter: CPython 3.14.8 (`/opt/homebrew/bin/python3`).

Capture (read-only toward the scripts):

```bash
cd crates/trusty-architect/tests/fixtures/dashboard/pending
python3 -I capture.py <architect-checkout>/scripts
```

Read by `tests/dashboard_pending.rs` and the `isotime` unit test.
#12029 P2e deleted the serde `pending`, `stale_tasks` and `last_activity`,
so `cases.json` and the `pending`, `stale_tasks` and `last_activity`
goldens are no longer read; the `dashboard_derive` and `owner_answers`
replay recordings cover the port.

#12029 P2g: the port's `pending` follows the
bundled copy's `_with_note` (#10765) on purpose: an `answered` event's text
is the answer, then `: <trimmed answer_note>` when the note is a non-blank
string, as the Architect Do text already was. The frozen
`dashboard_derive.py` (architect `0fbbc81eed`, line 491) drops the note on a
plain answer, so this is an intended divergence from the frozen reference,
not from these goldens (they were captured from the bundled copy). Pinned by
`answered_text_carries_the_trimmed_answer_note`; the `owner_answers` replay
records it as `ANSWER_NOTE_DIVERGENCE` in `tests/replay_owner_answers.rs`.
