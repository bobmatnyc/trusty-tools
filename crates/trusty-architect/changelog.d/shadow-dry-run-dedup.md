Fixed
- The scheduler's shadow poll now dedupes across cycles: `tm architect poll --dry-run` reads and saves its own state file when `ARCHITECT_DRY_RUN_STATE` names one, and the shadow plan seeds `local/meta/shadow/dry-run-state.json` once instead of copying the live state every run. Before, every shadow cycle re-alerted everything (new=347).
