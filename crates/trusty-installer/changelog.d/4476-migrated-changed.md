Changed

- **`detect_does_not_hang_on_an_unresponsive_shadowing_binary` now runs on
  Tokio's paused clock** — 10.01s to 0.00s. The hanging `sleep 30` shadowing
  binary is still really spawned and still really never answers; only the
  health gate's 10s probe timeout moves to the virtual clock. That the child
  is genuinely spawned under a paused clock was verified by measurement (a
  spawn marker written by the fake binary was present on 3 of 3 runs), and a
  new elapsed-time assertion keeps it honest: if `shadowing_version: None`
  ever came from an early probe failure instead of the timeout expiring, the
  test fails rather than passing green on deleted coverage. Reverting the
  probe to the un-timed-out `installed_version` call still fails the test.

---
