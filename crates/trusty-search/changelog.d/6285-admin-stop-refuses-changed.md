Changed
- `POST /admin/stop` now answers `503 shutdown_unavailable` (`retryable: false`)
  when no shutdown driver is listening, instead of reporting `ok: true` for a
  daemon that will keep running. A live daemon subscribes before it serves, so
  the refusal fires only when the stop genuinely cannot happen (#6285).
