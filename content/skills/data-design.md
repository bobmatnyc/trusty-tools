---
name: data-design
description: "Maps data type and question to chart form, encoding, colour scale, labels and interaction, with accessibility and dark-mode rules. Use when designing or reviewing any chart, dashboard, treemap, legend or table."
user-invocable: false
metadata:
  version: "0.1.0"
category: agent-reference
tags: [dataviz, charts, colour, accessibility, design]
effort: medium
---

# Data Design

Pick the chart from the data type and the reader's question, then the
encoding, then the colour. Read a reference file only when the task touches
its topic.

Channel accuracy, best first (Cleveland-McGill; Vega-Lite's channel ranking):
position on a common scale > length > angle or slope > area > colour
lightness > colour hue. Put the main quantity on the best channel available.

## Decision Table

| Data type / question | Form | Encoding | Colour | Interaction |
| --- | --- | --- | --- | --- |
| Categorical, compare counts | Sorted horizontal bar | Length from a zero baseline | One hue; highlight one bar | Sort, hover value |
| Categorical identity | Grouped bars or direct labels | Hue or position | Qualitative, max 7 hues, rest as "Other" | Legend toggle |
| Ordinal levels | Ordered bars, dot plot, heat strip | Position order | Sequential, one hue | Hover |
| One quantity | Bar, dot, lollipop; text for a single number | Length or position | Neutral | Tooltip |
| Part-to-whole, 5 parts or fewer | Stacked or 100% bar; pie only for 2-3 slices | Length; angle for pie | Qualitative or sequential | Hover share |
| Part-to-whole, many parts | Sorted bar, or treemap | Area, labelled with value | One hue per parent, tint by depth | Drill, hover |
| Hierarchy, size matters | Treemap | Area = measure, nesting = parent | Hue = parent, lightness = depth | Click, breadcrumbs |
| Hierarchy, structure matters | Indented tree; sunburst if shallow | Position and indent | Neutral | Expand |
| Time series, trend | Line; area for one series | x = time, y = value | One hue per series, direct labels | Crosshair, range |
| Time series, many series | Small multiples | Shared axes | One hue | Synced hover |
| Time, bucket totals by category | Stacked column | Length | Qualitative | Hover bucket |
| Flow, A to B | Sankey; table above ~30 links | Link width | Source hue | Hover path |
| Geography | Choropleth for rates; symbols for counts | Fill or symbol size | Sequential or diverging | Zoom |
| Network | Node-link under ~100 nodes; matrix above | Position; size = degree | Hue = community | Hover neighbours |
| Distribution | Histogram, box, strip or beeswarm | Position | One hue | Brush |
| Correlation | Scatter | x and y position | Hue only for a third category | Hover |
| Deviation from a target | Diverging bar | Length from zero | Diverging, neutral midpoint | Hover |
| Status (ok / warn / stop) | Text plus icon plus colour | Shape and label first | Status tokens, never the only cue | None |

## Hard Rules

1. **One meaning per colour or pattern** in a view. A texture that means
   "no goal" in one chart never means "no data" in the next.
2. **Never colour alone** (WCAG 1.4.1). Pair colour with a label, position
   or shape. Non-text marks need 3:1 against the surface (WCAG 1.4.11).
3. **Text wears text tokens.** Labels need 4.5:1 and use the ink colour,
   never the series colour.
4. **Area must encode data.** A mark sized by an invented weight is a false
   statement; list the item instead of drawing it.
5. **Zero, null, unknown and estimate are four states.** Zero is drawn,
   null and unknown are listed, an estimate is flagged in text.
6. **Patterns need room.** At most one hatch per page, one meaning, only on
   regions at least 24 px on the short side.
7. **Re-derive dark mode, never invert.** Validate every palette against the
   real surface colour in both themes with `scripts/check_palette.py`.
8. **Chart data never leaves the host.** Use no hosted chart service
   (Datawrapper, Flourish, any upload-to-render API) for real data.
9. **Every chart has a text or table equivalent** and respects
   `prefers-reduced-motion`.

## Colour in One Paragraph

Qualitative: at most 7 hues in a fixed OKLCH lightness band, CVD distance
of at least 8 between neighbours. Sequential: one hue, monotonic lightness.
Diverging: two hues around a neutral midpoint, only when zero or a target
is meaningful. Vendored ColorBrewer subsets live in
`references/palettes-colorbrewer.md`.

## Palette Check

```bash
python3 scripts/check_palette.py --surface '#fcfcfb' '#1b9e77' '#d95f02' '#7570b3'
python3 scripts/check_palette.py --surface '#1a1a19' --mode sequential '#deebf7' '#9ecae1' '#3182bd'
```

The script is stdlib-only and works offline. It reports CVD separation
(protan, deutan, tritan), the OKLCH lightness band, and contrast against the
surface. Exit 0 means every check passed; exit 1 lists each failure.

## Load Next (References)

- `references/chart-selection.md` — the full table with the question taxonomy.
- `references/colour.md` — scales, CVD, dark mode, validation procedure.
- `references/encoding-and-labels.md` — channels, labels, legends, annotation.
- `references/hierarchy-and-part-to-whole.md` — treemap, sunburst, pie limits.
- `references/time-flow-network-geo.md` — time, Sankey, networks, maps.
- `references/empty-and-missing-data.md` — zero, null, unknown, estimate.
- `references/accessibility.md` — WCAG checks, fallbacks, motion.
- `references/palettes-colorbrewer.md` — vendored hex subsets (Apache-2.0).
- `references/PROVENANCE.md` — what is vendored and from where.
- `references/LICENSE-colorbrewer.txt` — the Apache-2.0 text for the subsets.
