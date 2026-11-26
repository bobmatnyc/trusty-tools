Added
- `tm architect restore [--version V] [--dir DIR]` puts back the files the last launch deploy replaced, from the kept copy. A file holding the bytes that deploy wrote, or absent, gets the kept bytes and mode back; an edited file is left as is and reported (exit 1). It writes files only and restarts nothing (Refs #11368).
