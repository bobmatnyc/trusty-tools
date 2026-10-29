# Empty and Missing Data

Four different states. Each gets its own treatment, and none of them is
drawn as if it were a measured value.

| State | Meaning | Show it as | Never |
| --- | --- | --- | --- |
| Zero | Measured, and the value is 0 | Drawn: an empty bar with a "0" label, a tick at the baseline | Hidden, or confused with missing |
| Null / not recorded | No measurement exists | Listed: count in the header, items in a panel or strip | A tiny mark with an invented size |
| Unknown | A measurement exists but cannot be attributed | A labelled "Unattributed" group, neutral colour | Spread across known groups |
| Estimate | Derived or projected value | Drawn, flagged in text ("estimate") and in the tooltip | A texture on marks too small to show it |

## Rules

- **Do not invent a size.** A leaf with no value gets no area. A floor
  weight (for example total / 4000) produces slivers that say nothing.
- **Count what is missing, out loud.** "41 of 300 items have no token data
  and are listed, not drawn."
- **Line charts:** a missing bucket is a gap; a zero bucket touches zero.
  Joining across a gap claims data you do not have.
- **Tables:** use an em dash for null, "0" for zero, and say which is which
  in the footnote.
- **Empty state of a whole chart:** replace the chart with one sentence that
  says why it is empty and what would fill it. No empty axes.
- **Partial periods:** flag the current bucket as partial; do not compare it
  to full buckets without saying so.

## Sources

- Datawrapper Academy on missing data (paraphrased): https://academy.datawrapper.de/
- Vega-Lite invalid-data handling (BSD-3-Clause): https://vega.github.io/vega-lite/docs/invalid-data.html
