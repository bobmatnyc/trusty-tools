Added
- `search.project.ensure` (MCP `ensure_project`) finds or creates the one pinned index of a trusty-mpm project and queues its first reindex once; a repeat call returns the same id and queues nothing. A root that contains, or sits inside, a registered root is refused with `project_root_overlap` naming the overlapping index id (#10531).
