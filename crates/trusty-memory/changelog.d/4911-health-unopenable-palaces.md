Added

- `GET /health` reports `unopenable_palaces` — the id and reason for every palace present on disk that startup hydration refused. The key is omitted when there are none, so a healthy daemon's payload is unchanged (#4911).
