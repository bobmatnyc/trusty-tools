Fixed
- `tm hook --pm-guard` no longer refuses a Python script that assigns a local variable named `ps` (`ps = cell.paragraphs`); a real `ps e`/`ps -E` environment listing is still refused (#10688).
