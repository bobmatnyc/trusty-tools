Fixed
- `tm hook --pm-guard` now refuses `file -f`, `--files-from`, `-m` and `--magic-file` when they name a secret-bearing file such as `.env`, because `file` echoes each line of such a list in its output. Plain `file .env` stays allowed (#9791).
