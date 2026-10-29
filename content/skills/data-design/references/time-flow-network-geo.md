# Time, Flow, Network and Geography

## Time

- Time on x, left to right, evenly spaced by real time. Missing buckets are
  gaps, not a joined line (see `empty-and-missing-data.md`).
- Line for continuous trends; columns for a few discrete buckets.
- More than ~7 series: small multiples with shared axes, or highlight one and
  grey the rest.
- Mark a partial current period ("to date") and an incomplete bucket.
- Smoothing, moving averages and log scales are stated in the subtitle.
- Stacked areas hide every series except the bottom one; use them for totals.

## Flow

- Sankey for a few stages and up to ~30 links; link width = amount.
- Colour links by source (or by outcome), one rule per chart.
- Above ~30 links, show a sorted flow table or a matrix.
- Flows must conserve: what enters a node leaves it, or the gap is labelled.

## Network

- Node-link under ~100 nodes; an adjacency matrix above that.
- Position comes from the layout; size = degree or a named measure.
- Hue = community or type, max 7; edges are light and thin.
- Fix the layout seed so the picture is stable between renders.
- Hover shows neighbours; never rely on spotting a single edge in a hairball.

## Geography

- Choropleth only for rates or normalised values; counts get proportional
  symbols (circle area = count).
- Sequential scale for magnitude, diverging only around a meaningful centre.
- Classify with 5-7 classes; state the method (quantile, equal interval).
- Large, empty regions dominate a choropleth; consider a tile grid map.
- Do not send coordinates or fleet data to a hosted map or tile service;
  render from local geometry.

## Sources

- FT Visual Vocabulary, flow and spatial (MIT): https://github.com/Financial-Times/chart-doctor/tree/main/visual-vocabulary
- Observable Plot (ISC): https://observablehq.com/plot/
- ColorBrewer, choropleth guidance (Apache-2.0): https://colorbrewer2.org/
