Fixed
- `run_bounded_python_check` no longer reports a venv recheck as `Failed`
  when the spawn itself is starved by CI contention (EAGAIN/ENOMEM) — the
  check budget now covers the spawn attempt, and a transient spawn error
  retries within it instead of returning a false-negative failure (#5328).
