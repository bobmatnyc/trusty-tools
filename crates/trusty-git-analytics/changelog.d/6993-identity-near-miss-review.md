Added

- `tga aliases suggest --review-file <PATH>` writes the near-miss identity pairs —
  the ones scoring below `--confidence` but at or above 0.50, which the normal
  output never shows — to a tab-separated file with both addresses, the reason, the
  confidence, and a `confirmed` column the operator sets to `yes`. Stdout still
  carries only the at-or-above-threshold suggestions, and no file is written unless
  the flag is given (#6993).
