Fixed

- `tm hook --pm-guard`'s cost tests no longer race the guard's 200 ms
  production read deadline. Expiry fails open, so a scheduling stall on a CI
  runner reported 0 tokens against an expected 71540 and reddened main; the
  same deadline let `still_fails_open_when_even_the_larger_tail_has_no_record`
  pass without reading its fixture at all. The deadline is now a parameter the
  suite supplies, the production path is unchanged at 200 ms, and the answer
  the guard gives when no measurement arrives is pinned on a pure mapping
  rather than on the clock (#7028).
