Changed
- `tm secrets set` confirms a value with the mask of the owner ruling of 2026-10-28 (#10394): the first 4 and last 5 characters and the length for 16 characters or more, the last 5 and the length for 10 to 15, the length alone under 10. It no longer prints the first 8 characters of a stored value. The `--help` text says so.
