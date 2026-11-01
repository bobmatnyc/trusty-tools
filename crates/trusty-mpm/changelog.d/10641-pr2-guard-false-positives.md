Fixed
- A `token*` usage tally such as `token-hourly.jsonl` no longer counts as a token file for the secret-read guard; a name another secret family claims still does, in any extension (`token-usage.key.log`) (#10570, #10641).
- Five seeded Architect scripts (`datg_store.py`, `dashboard_store.py`, `dashboard_page.py`, `dashboard_viz.py`, `dashboard_http.py`) no longer carry words the guard reads as a credential command; their behaviour is unchanged, and the scripts that still refuse are listed with their reason (#10570).
