Fixed
- `tm hook --pm-guard` now refuses a Bash segment whose shell operators it cannot place in the script-body, computed-read and secret-copy rules, instead of reading that segment as having no words; inside a script body such a segment refuses when it runs a script or names a secret-shaped word (#11276).
