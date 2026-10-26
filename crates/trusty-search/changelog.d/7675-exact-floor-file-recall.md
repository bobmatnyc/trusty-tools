Fixed
- Default hybrid `search` no longer drops files that carry a quoted string, identifier or issue reference when one file has several overlapping chunks carrying it. Inside the exact-match floor, each carrying file's best chunk now ranks before any file's second chunk, so every file ripgrep finds fits a page sized to its matching lines. The top hit is unchanged (#7675).
