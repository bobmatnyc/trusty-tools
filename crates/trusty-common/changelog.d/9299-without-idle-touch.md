Added
- `PalaceHandle::without_idle_touch` runs a future with `PalaceHandle::touch` suppressed, so a cross-palace fan-out can search palaces without resetting their idle clocks (#9299, ADR-0071 D2).
