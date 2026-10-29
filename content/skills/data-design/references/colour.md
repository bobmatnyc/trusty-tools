# Colour

Colour is the weakest channel for quantity and the strongest for identity.
Spend it on one meaning per view.

## Scale Types

| Scale | Data | Build | Example (vendored) |
| --- | --- | --- | --- |
| Qualitative | Nominal identity | Distinct hues at similar lightness | Set2, Dark2 |
| Sequential | Ordered magnitude | One hue (or a short hue path), lightness changes monotonically | Blues, Oranges, YlGnBu |
| Diverging | Signed values around a meaningful midpoint | Two hues, light neutral midpoint | RdBu, PuOr, BrBG |
| Status | ok / warn / stop | Fixed tokens, always paired with text or icon | Project tokens |

Hex values: `palettes-colorbrewer.md`. The Okabe-Ito eight-colour set is a
second widely used colour-blind-safe qualitative set; cite it and take its
values from the published guideline.

## Rules

- **Qualitative:** at most 7 hues. Keep OKLCH lightness inside one band
  (the checker's default is 0.48-0.67 for marks on a light surface) so no
  series looks more important by accident. Group the tail into "Other".
- **Sequential:** lightness must change in one direction. Light means less
  on a light surface; on a dark surface, reverse so more data is brighter.
- **Diverging:** only when zero or a target means something. The midpoint
  colour is neutral and lighter than both ends.
- **Highlight:** one saturated hue for the focus, grey for the rest.
- **Tints carry depth, not data.** A `color-mix` tint of a series hue may
  show nesting depth; it must not encode a second quantity.
- **Same thing, same colour** across every chart on a page.

## Colour-Vision Deficiency

About 1 in 12 men and 1 in 200 women have a red-green deficiency. Check
every qualitative palette under protan, deutan and tritan simulation. The
checker uses the Machado, Oliveira and Fernandes (2009) matrices at full
severity and reports the smallest pairwise OKLab distance (x100). Target at
least 8 under simulation and 15 with normal vision.

## Dark Mode

Re-derive the palette, never invert it. On a dark surface (for example
`#1a1a19`) raise lightness and lower chroma slightly. Gaps and strokes
between marks use the surface colour, not white. Run the checker once per
theme against the real surface hex.

## Validation Procedure

1. List the series hexes in display order.
2. Run `python3 scripts/check_palette.py --surface <hex> <colours...>` for
   each theme. Add `--mode sequential` for an ordered scale.
3. Fix every failure: move lightness into the band, swap the closest pair,
   or add a non-colour cue (label, shape) when hues must stay.
4. Paste the command and its output into the design spec.

## Sources

- ColorBrewer (Apache-2.0): https://colorbrewer2.org/
- Okabe and Ito, "Color Universal Design" (2008): https://jfly.uni-koeln.de/color/
- Machado, Oliveira, Fernandes (2009), IEEE TVCG 15(6), CVD simulation.
- OKLab / OKLCH, Bjorn Ottosson (2020): https://bottosson.github.io/posts/oklab/
