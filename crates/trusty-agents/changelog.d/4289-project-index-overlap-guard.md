Fixed
- `POST /api/projects` and `POST /api/project-tools/index` now refuse a directory that overlaps a project or search index already registered — the same tree under another spelling, a subdirectory of one, or an ancestor that would enclose one — and the `409` names the existing project or index (#4289).
- A registry that already contains overlapping project roots still loads in full; the overlaps are reported at startup instead of rejected.
- `POST /api/project-tools/index` now reports trusty-search's own overlap refusal as `409 {error, existing_index_id}` instead of a generic `503`, so the GUI can offer to attach to the existing index (#4289).
