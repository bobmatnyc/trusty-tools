Fixed

- **The standalone authorship report now states why it has no data**
  ([#6046](https://github.com/bobmatnyc/trusty-tools/issues/6046)).
  A repository whose authorship artifact fails to load renders the section as
  `_No data available — see Gaps & Caveats._`, and since the split that section
  lives in the code-review report — so a reader holding only the authorship
  document saw the collapse line and no reason for it. The authorship-load gap
  lines now render as a `Data gaps in this assessment` block above the body.
  Gaps from other legs stay out of it, and a run whose authorship leg succeeded
  is byte-identical to before.
