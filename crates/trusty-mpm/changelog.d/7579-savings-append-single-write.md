Fixed
- The savings ledger writes a row and its newline in one `write_all` instead of two `O_APPEND` writes, so two racing producers can no longer interleave bytes within a row. (#7579)
