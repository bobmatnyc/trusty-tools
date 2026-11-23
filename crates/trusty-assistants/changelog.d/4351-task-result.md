Added

- `dispatch_task` relays the coding backend's actionable result — the ref, the branch and a pass/fail classification — on the proposal envelope, instead of returning only a transcript (#4351).
- The classification comes from the child's exit code, so a run that stopped at its turn budget with real work on disk reports `partial` rather than being discarded as a failure.
- A backend that reports no result leaves the envelope byte-identical to before.
