Fixed
- `tm hook --pm-guard` now refuses a Bash segment whose shell operators it cannot place in the script-body, computed-read and secret-copy rules, instead of reading that segment as having no words (#11276).
