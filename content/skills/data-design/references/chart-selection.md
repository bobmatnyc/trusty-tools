# Chart Selection

Start from the question the reader asks, not from the chart you like. Name
the question in one sentence, classify each field, then pick the form.

## Step 1: Classify Each Field

| Type | Examples | Order? | Arithmetic? |
| --- | --- | --- | --- |
| Nominal (categorical) | agent name, crate, goal | No | No |
| Ordinal | severity, size bucket, rank | Yes | No |
| Quantitative | tokens, duration, count | Yes | Yes |
| Temporal | timestamp, day, week | Yes | Intervals |

The four types follow Vega-Lite's data-type vocabulary. A numeric code that
names a category (an exit code, an issue number) is nominal, not
quantitative.

## Step 2: Name the Relationship

These families paraphrase the FT Visual Vocabulary taxonomy.

| Relationship | Reader asks | First choice | Also good | Avoid |
| --- | --- | --- | --- | --- |
| Deviation | How far from a target or zero? | Diverging bar | Spine chart | Two separate bars |
| Correlation | Do two measures move together? | Scatter | Connected scatter, XY heatmap | Dual-axis line |
| Ranking | Which is biggest? | Sorted bar | Dot strip, lollipop | Unsorted bar, pie |
| Distribution | How are values spread? | Histogram | Box, strip, beeswarm | Mean alone |
| Change over time | What is the trend? | Line | Column for few buckets, small multiples | 3D, smoothing without saying so |
| Magnitude | How big, compared? | Bar from zero | Paired bar, proportional symbol | Truncated axis on bars |
| Part-to-whole | What share of the total? | Stacked or 100% bar | Treemap for many parts | Pie with more than 3 slices |
| Spatial | Where? | Choropleth (rates) | Proportional symbol (counts) | Choropleth of raw counts |
| Flow | How much moves from A to B? | Sankey | Chord, flow table | Spaghetti node-link |

## Step 3: Check the Form Against the Data

- **Count the marks.** Over ~12 bars, sort and cut to a top N plus "Other".
  Over ~7 lines, switch to small multiples or highlight one line.
- **Check the baseline.** Bars and areas start at zero. Lines and dots may
  zoom, and the axis says so.
- **Check the denominator.** A map or part-to-whole chart of raw counts
  often shows population, not the effect. Normalise first.
- **One chart, one question.** Two questions get two charts sharing axes.
- **A number is a chart.** One key value is a stat tile with its unit and
  comparison period, not a gauge.

## Step 4: Write the Spec

The design spec names: question, fields and types, form, encoding per
channel, colour tokens, label rules, empty-state rules, interaction,
accessibility checks, and the palette-check command with its output.

## Sources

- FT Visual Vocabulary (MIT): https://github.com/Financial-Times/chart-doctor/tree/main/visual-vocabulary
- Vega-Lite data types (BSD-3-Clause): https://vega.github.io/vega-lite/docs/type.html
- Cleveland and McGill, "Graphical Perception" (1984), JASA 79(387).
