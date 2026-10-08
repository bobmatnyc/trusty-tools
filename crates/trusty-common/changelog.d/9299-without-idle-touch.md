Added
- `PalaceHandle::without_idle_touch` runs a future with `PalaceHandle::touch` suppressed, so a cross-palace fan-out can search palaces without resetting their idle clocks (#9299, ADR-0071 D2).
- `recall_across_palaces_reporting` runs the same fan-out as `recall_across_palaces` and returns a `CrossPalaceRecall` that also names the palaces whose recall errored, so a caller can report them instead of counting them as searched. `recall_across_palaces` is unchanged (#9299).
