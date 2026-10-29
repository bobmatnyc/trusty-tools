<!--
Provenance (vendored data, listed in PROVENANCE.md):
  Source repo:   axismaps/colorbrewer
  Source path:   colorbrewer_schemes.js
  Pinned commit: 7d135fc4e19eda73f2eb1bf55fcdf4a04fe4881f
  SPDX licence:  Apache-2.0
  Retrieved:     2026-10-28
  Status:        adapted (subset of schemes and class counts; rgb() converted
                 to hex; colour values unchanged)
NOTICE (retained verbatim from the source file):
  This product includes color specifications and designs developed by Cynthia
  Brewer (http://colorbrewer.org/).
-->

# ColorBrewer Palette Subsets

Hex values from ColorBrewer by Cynthia Brewer and Axis Maps, Apache-2.0.
Only the schemes below are vendored. Sequential and diverging rows run from
the low end to the high end. Validate any use with `scripts/check_palette.py`
against the real surface colour: ColorBrewer was designed for print and
light maps, so a dark theme needs a re-derived ramp.

## Sequential (single hue)

| Scheme | Classes | Hex (light to dark, or first to last) |
| --- | --- | --- |
| Blues | 3 | `#deebf7` `#9ecae1` `#3182bd` |
| Blues | 5 | `#eff3ff` `#bdd7e7` `#6baed6` `#3182bd` `#08519c` |
| Blues | 7 | `#eff3ff` `#c6dbef` `#9ecae1` `#6baed6` `#4292c6` `#2171b5` `#084594` |
| Blues | 9 | `#f7fbff` `#deebf7` `#c6dbef` `#9ecae1` `#6baed6` `#4292c6` `#2171b5` `#08519c` `#08306b` |
| Oranges | 3 | `#fee6ce` `#fdae6b` `#e6550d` |
| Oranges | 5 | `#feedde` `#fdbe85` `#fd8d3c` `#e6550d` `#a63603` |
| Oranges | 7 | `#feedde` `#fdd0a2` `#fdae6b` `#fd8d3c` `#f16913` `#d94801` `#8c2d04` |
| Oranges | 9 | `#fff5eb` `#fee6ce` `#fdd0a2` `#fdae6b` `#fd8d3c` `#f16913` `#d94801` `#a63603` `#7f2704` |

## Sequential (multi-hue)

| Scheme | Classes | Hex (light to dark, or first to last) |
| --- | --- | --- |
| YlGnBu | 3 | `#edf8b1` `#7fcdbb` `#2c7fb8` |
| YlGnBu | 5 | `#ffffcc` `#a1dab4` `#41b6c4` `#2c7fb8` `#253494` |
| YlGnBu | 7 | `#ffffcc` `#c7e9b4` `#7fcdbb` `#41b6c4` `#1d91c0` `#225ea8` `#0c2c84` |
| YlGnBu | 9 | `#ffffd9` `#edf8b1` `#c7e9b4` `#7fcdbb` `#41b6c4` `#1d91c0` `#225ea8` `#253494` `#081d58` |

## Diverging

| Scheme | Classes | Hex (light to dark, or first to last) |
| --- | --- | --- |
| RdBu | 3 | `#ef8a62` `#f7f7f7` `#67a9cf` |
| RdBu | 5 | `#ca0020` `#f4a582` `#f7f7f7` `#92c5de` `#0571b0` |
| RdBu | 7 | `#b2182b` `#ef8a62` `#fddbc7` `#f7f7f7` `#d1e5f0` `#67a9cf` `#2166ac` |
| RdBu | 9 | `#b2182b` `#d6604d` `#f4a582` `#fddbc7` `#f7f7f7` `#d1e5f0` `#92c5de` `#4393c3` `#2166ac` |
| RdBu | 11 | `#67001f` `#b2182b` `#d6604d` `#f4a582` `#fddbc7` `#f7f7f7` `#d1e5f0` `#92c5de` `#4393c3` `#2166ac` `#053061` |
| PuOr | 3 | `#f1a340` `#f7f7f7` `#998ec3` |
| PuOr | 5 | `#e66101` `#fdb863` `#f7f7f7` `#b2abd2` `#5e3c99` |
| PuOr | 7 | `#b35806` `#f1a340` `#fee0b6` `#f7f7f7` `#d8daeb` `#998ec3` `#542788` |
| PuOr | 9 | `#b35806` `#e08214` `#fdb863` `#fee0b6` `#f7f7f7` `#d8daeb` `#b2abd2` `#8073ac` `#542788` |
| PuOr | 11 | `#7f3b08` `#b35806` `#e08214` `#fdb863` `#fee0b6` `#f7f7f7` `#d8daeb` `#b2abd2` `#8073ac` `#542788` `#2d004b` |
| BrBG | 3 | `#d8b365` `#f5f5f5` `#5ab4ac` |
| BrBG | 5 | `#a6611a` `#dfc27d` `#f5f5f5` `#80cdc1` `#018571` |
| BrBG | 7 | `#8c510a` `#d8b365` `#f6e8c3` `#f5f5f5` `#c7eae5` `#5ab4ac` `#01665e` |
| BrBG | 9 | `#8c510a` `#bf812d` `#dfc27d` `#f6e8c3` `#f5f5f5` `#c7eae5` `#80cdc1` `#35978f` `#01665e` |
| BrBG | 11 | `#543005` `#8c510a` `#bf812d` `#dfc27d` `#f6e8c3` `#f5f5f5` `#c7eae5` `#80cdc1` `#35978f` `#01665e` `#003c30` |

## Qualitative

| Scheme | Classes | Hex (light to dark, or first to last) |
| --- | --- | --- |
| Set2 | 8 | `#66c2a5` `#fc8d62` `#8da0cb` `#e78ac3` `#a6d854` `#ffd92f` `#e5c494` `#b3b3b3` |
| Dark2 | 8 | `#1b9e77` `#d95f02` `#7570b3` `#e7298a` `#66a61e` `#e6ab02` `#a6761d` `#666666` |
