Added
- `GET /api/console/services` carries a `lifecycle` field on every row — `"daemon"` or `"on_demand"` (#6416). A payload written before this reads as `"daemon"`, which is what it was
