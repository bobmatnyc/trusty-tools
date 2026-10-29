# Accessibility

## Checks Every Chart Passes

| Check | Standard | Test |
| --- | --- | --- |
| Information is not carried by colour alone | WCAG 2.2 SC 1.4.1 | Grey-scale the chart; can the reader still answer the question? |
| Non-text marks contrast with the surface | SC 1.4.11, 3:1 | `check_palette.py --surface <hex>` |
| Text contrast | SC 1.4.3, 4.5:1 (3:1 for 18 px+ or 14 px bold) | Check label colour against the fill behind it |
| CVD separation | Project target dE >= 8 | `check_palette.py` protan, deutan, tritan |
| Text equivalent exists | SC 1.1.1 | A table, or a sentence with the key numbers |
| Keyboard reach | SC 2.1.1 | Every interactive mark is focusable, with a visible focus ring |
| Motion | SC 2.3.3, `prefers-reduced-motion` | Transitions off or instant when the user asks |

## Patterns

- Redundant cues first: direct labels, position, shape, line dash.
- Hatching is a last resort: one pattern per page, one meaning, and only on
  regions at least 24 px on the short side.
- Line series also differ by dash or marker when there are more than 3.

## Text Equivalents

- Every chart has an `aria-label` or caption stating the takeaway.
- Complex charts link to a data table with the same numbers.
- Tooltips are not the only place a value lives; keyboard and touch users
  must reach it too.

## Theme Parity

Run every check in both light and dark themes, against the actual surface
colour of each.

## Sources

- WCAG 2.2: https://www.w3.org/TR/WCAG22/
- W3C WAI, complex images: https://www.w3.org/WAI/tutorials/images/complex/
