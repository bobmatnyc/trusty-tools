Security
- Spreadsheet extraction now uses calamine 0.37, which reads `.xlsx`/`.xlsm` packages with the patched zip 9.0.5. calamine 0.36 pulled zip 8.6.0, which is inside the range of GHSA-m665-v7pm-7grj (a zip parser differential, `zip <= 9.0.4`).
