Fixed
- The savings ledger no longer accumulates duplicate `instruction-compression`
  rows: every producer now appends through one choke point that reads the ledger
  first and writes only when no row with the same session, technique and basis is
  already recorded. One live session had 312 byte-identical rows. An unreadable
  ledger skips the write and logs it rather than appending blind, and
  `tm repair savings-ledger` collapses existing duplicates to the earliest copy
  of each measurement (dry run by default, `--apply` to quarantine and rewrite).
