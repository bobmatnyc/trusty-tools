Fixed
- `POST /indexes` now stamps a newly created, empty corpus at the current schema version. An unstamped index read back as schema 0 and re-ran every migration on its first restart, including the M005 corpus clear and re-chunk (#8777).
