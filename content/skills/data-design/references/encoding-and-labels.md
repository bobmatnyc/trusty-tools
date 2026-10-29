# Encoding and Labels

## Channel Ranking

| Rank | Channel | Good for |
| --- | --- | --- |
| 1 | Position on a common scale | Any quantity |
| 2 | Position on unaligned scales | Small multiples |
| 3 | Length | Bars |
| 4 | Angle, slope | Pie (few slices), slope charts |
| 5 | Area | Treemap, bubble (labelled) |
| 6 | Colour lightness | Ordered magnitude, heatmaps |
| 7 | Colour hue | Category identity only |

This order paraphrases Cleveland-McGill and Vega-Lite's effectiveness
ranking. Map the most important field to the highest free channel.

## Labels

- **Direct labels beat legends.** Put the series name at the end of its
  line or inside its bar. Use a legend only when marks are too small.
- **Label text uses the ink token** and meets 4.5:1 against what is behind
  it, including a tinted fill.
- **Show a label only where it fits.** Set a minimum box (for example
  56 x 22 px for a treemap tile); below it, the value moves to the tooltip.
- **Units once.** Put the unit in the axis title or the first label, not on
  every tick.
- **Round to what the reader can use.** Two significant figures for most
  dashboards; abbreviate with k, M, and say so.

## Axes and Gridlines

- Bars start at zero. Say so when a line chart's axis does not.
- Gridlines are light and few; they support reading, they are not data.
- Time runs left to right. Do not rotate tick labels; thin them instead.

## Legends

- Order legend entries in the order marks appear.
- A legend swatch means the same thing everywhere it appears on the page.
- Remove a legend entry for a mark the chart no longer draws.

## Annotation

- One sentence of takeaway above the chart beats a neutral title.
- Annotate the event that explains a change, at the point it happens.
- Mark estimates and partial periods in text ("estimate", "to date").

## Small Marks

- Thin slices, slivers and 1 px bars cannot carry a pattern, a label or a
  border. Merge them, list them, or give them their own view.
- Separate adjacent fills with a 1-2 px gap in the surface colour.

## Sources

- Vega-Lite encoding (BSD-3-Clause): https://vega.github.io/vega-lite/docs/encoding.html
- Observable Plot marks and scales (ISC): https://observablehq.com/plot/
- Datawrapper Academy on labels and legends (paraphrased, not copied): https://academy.datawrapper.de/
