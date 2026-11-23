Fixed
- A reindex refused for exceeding the file or byte budget no longer marks the index's Ready lexical, semantic and graph stages `failed`. Those lanes keep serving, so search stays on the vector path; the refusal is still recorded in `last_walk_error` and sent as the reindex's fatal error event (#10692).
- After a refused walk, `GET /indexes/:id/status` reads `status: "degraded"` and adds `walk_refused` and `degraded_reason`; `/health` reads `degraded` and adds `indexes_walk_refused` (index id to reason). The next walk clears both (#10692).
