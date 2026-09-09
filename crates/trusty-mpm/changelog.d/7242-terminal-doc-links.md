Fixed

- Three doc comments in `tui::terminal` linked to a `leave` function that no
  longer exists — #7248 renamed the pair to `suspend`/`resume` and left the
  references behind, so rustdoc rendered them as dead literal text and the
  `Rustdoc intra-doc links` gate counted two broken links against a crate whose
  baseline is zero. The module doc also told a caller to re-enter with `enter`,
  which would build a second terminal instead of taking the existing one back;
  it now names `resume`. Documentation only — no behaviour changes.
