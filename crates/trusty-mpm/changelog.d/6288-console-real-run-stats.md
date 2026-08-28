Fixed
- `console_metrics` and `supervisor_status` now report the supervisor's real `run_stats` — sweeps, auto-resumes, resume failures, classifications — instead of a permanent zero. A missing, corrupt, or stale snapshot is reported in a new `supervisor_metrics` field rather than as a silent zero (Refs #6288).
