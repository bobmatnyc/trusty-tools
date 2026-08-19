Fixed

- `tm hook --pm-guard` no longer charges a read-only here-document command
  against the PM's per-turn file-change budget
  (closes [#5356](https://github.com/bobmatnyc/trusty-tools/issues/5356)).
  The redirection check treated any unquoted `>` as a file write, and its quote
  scan knows only `'` and `"` — so a `>` in here-document body text, such as a
  `len(k) > 3` comparison in a `python3 <<'PY'` script or an `->` arrow in
  `cat <<EOF` prose, read as a redirect. Three such reads were allowed silently
  while consuming the budget, and the fourth was denied as "PM file-change
  budget 3/3 used this turn" with zero files changed. Only the body is treated
  as data: `python3 <<'PY' > out.rs` is still a file write, still consumes the
  budget, and still exhausts it.
