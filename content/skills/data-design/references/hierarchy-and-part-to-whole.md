# Hierarchy and Part-to-Whole

## Choosing

| Situation | Form | Why |
| --- | --- | --- |
| 2-3 parts, share matters | Pie or donut | Angles read well only for halves and quarters |
| Up to ~5 parts | 100% stacked bar | Length on a common baseline |
| Many parts, exact ranking matters | Sorted bar | Position beats area |
| Many parts, size and nesting both matter | Treemap | Area plus containment |
| Structure matters more than size | Indented tree, icicle | Position shows depth |
| Shallow hierarchy (2-3 levels), overview | Sunburst | Compact, but angles are hard to compare |

## Treemap Rules

- **Area = one measure.** Every tile's area comes from that measure. A leaf
  with no value is not tiled; list it in the panel or a strip below.
- **Hue = parent, lightness = depth.** Never use hue for the parent and also
  for a second meaning.
- **Order:** squarified layout, largest top-left, stable between refreshes so
  tiles do not jump.
- **Labels:** name plus value where the tile fits the label box; tooltip
  otherwise. Text uses the ink token over the tinted fill (check 4.5:1).
- **Gaps:** 1-2 px in the surface colour between tiles, a slightly larger gap
  between parents.
- **Caption:** say what area means and what is not drawn ("Area = output
  tokens. 41 items have no token data and are listed, not drawn.").
- **Interaction:** click to zoom into a parent, breadcrumbs to return.

## Stacked Bars

- Put the most important segment on the baseline; only it is easy to compare.
- At most ~5 segments; group the rest as "Other".
- 100% stacks hide totals; show the total beside the bar.

## Pies and Donuts

- Only for 2-3 slices that sum to a meaningful whole.
- Start at 12 o'clock, largest first, clockwise.
- Label slices directly with the share; no legend.

## Sources

- FT Visual Vocabulary, part-to-whole (MIT): https://github.com/Financial-Times/chart-doctor/tree/main/visual-vocabulary
- Bruls, Huizing, van Wijk, "Squarified Treemaps" (2000).
- Datawrapper Academy on pies and treemaps (paraphrased): https://academy.datawrapper.de/
