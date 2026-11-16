Fixed
- The Architect dashboard reads TOML on Python < 3.11: a shipped `dashboard_toml.py` reader stands in for `tomllib`, so the Projects stack and config rows fill on macOS's Python 3.9. The "needs Python 3.11" warning is removed; 3.11+ still uses `tomllib`.
