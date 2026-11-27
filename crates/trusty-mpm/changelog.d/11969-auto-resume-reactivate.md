Fixed
- An auto-resume of a decommissioned project's session now clears the project's `decommissioned_at` and `archived_at` by the same bound-project rule a new session uses. A reactivation or index-unpark failure is logged at WARN naming the project, and the session keeps running (#11969).
- Registering a decommissioned or archived project (MCP `project_register`, `POST /api/v1/projects`, `mpm.projects.registry.register`) still leaves it inactive, and the response now carries `inactive_reason`, which says to start a session to reactivate it. The key is absent for an active project (#11969).
