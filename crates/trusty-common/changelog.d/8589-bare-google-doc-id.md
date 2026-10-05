Fixed
- The memory secret filter stores a bare Google Docs/Sheets/Drive document id when the same text names a Google document (`spreadsheet`, `sheet`, `doc`, `drive`, `folder`, …) and the token has Google's exact id shape. A bare id with no such word, and every credential shape tested beside those words, is still refused (#8589).
