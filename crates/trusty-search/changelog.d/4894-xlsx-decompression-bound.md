Fixed

- A crafted spreadsheet can no longer force unbounded decompression: xlsx/xlsm/ods packages are now capped at 256 MiB of total uncompressed content before calamine opens them (closes [#4894](https://github.com/bobmatnyc/trusty-tools/issues/4894))
  - a 511 KB adversarial workbook used to extract *successfully* in 143 ms while peaking at 521 MiB RSS. `MAX_OFFICE_FILE_BYTES` (10 MiB) bounds the container rather than the decompressed payload, and `EXTRACT_TIMEOUT` (30 s) is a time bound the attack never approaches — so neither existing mitigation applied
  - calamine exposes no size limit and the zip layer bounds an entry read by its *compressed* length, so the check runs outside calamine: declared sizes are summed from the central directory first (rejecting a declared bomb with zero decompression), then every entry is drained through `Read::take` into a sink so a lying size field is caught too. The guard itself allocates O(1) regardless of the cap
  - measured on the same fixture: peak RSS 529.7 MiB before, 11.7 MiB after
  - the cap is package-wide rather than per-part like the docx path because calamine reads the whole package, and because a dense workbook at the container cap decompresses to ~190 MiB almost entirely within one sheet entry — a 50 MiB per-part cap would reject legitimate files
  - `.xls` is unaffected: CFB has no stream compression, so the container cap already bounds it
