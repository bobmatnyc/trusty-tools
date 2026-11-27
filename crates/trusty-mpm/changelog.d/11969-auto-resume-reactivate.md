Fixed
- An auto-resume of a decommissioned project's session now clears the project's `decommissioned_at` and `archived_at` by the same bound-project rule a new session uses. A reactivation or index-unpark failure is logged at WARN naming the project, and the session keeps running (#11969).
