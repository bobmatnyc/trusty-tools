Fixed
- A reindex refused for exceeding the file or byte budget no longer marks the index's Ready lexical, semantic and graph stages `failed`. Those lanes keep serving, so search stays on the vector path; the refusal is still recorded in `last_walk_error` and sent as the reindex's fatal error event (#10692).
