# Provenance

Every file in this skill is written for trusty-tools except the entries in
the table below. An update to a vendored file is a deliberate PR that bumps
the pinned commit; nothing here syncs automatically.

## Vendored Files

| File | Source repo | Source path | Pinned commit | SPDX | Retrieved | Status |
| --- | --- | --- | --- | --- | --- | --- |
| `palettes-colorbrewer.md` | axismaps/colorbrewer | `colorbrewer_schemes.js` | `7d135fc4e19eda73f2eb1bf55fcdf4a04fe4881f` | Apache-2.0 | 2026-10-28 | adapted: subset, rgb() to hex, values unchanged |
| `LICENSE-colorbrewer.txt` | axismaps/colorbrewer | `LICENCE.txt` | `7d135fc4e19eda73f2eb1bf55fcdf4a04fe4881f` | Apache-2.0 | 2026-10-28 | verbatim |

The source file's attribution notice is retained in the header of
`palettes-colorbrewer.md` and in the repository's `THIRD_PARTY_NOTICES.md`.
The upstream repository ships no separate `NOTICE` file.

## Cited, Not Copied

These sources informed the prose. Their text is paraphrased; none is
vendored.

| Source | Licence | Used for |
| --- | --- | --- |
| FT Visual Vocabulary (Financial-Times/chart-doctor) | MIT | Relationship taxonomy in `chart-selection.md` |
| Vega-Lite | BSD-3-Clause | Data types, channel ranking |
| Observable Plot | ISC | Mark and scale terminology |
| Okabe-Ito palette | Published guideline | Named as a qualitative option; values not vendored |
| Datawrapper Academy | Copyrighted, no OSS licence | Principles paraphrased and linked; no text copied |
| Machado, Oliveira, Fernandes (2009) | Published paper | CVD matrices in `scripts/check_palette.py` |
| OKLab (Bjorn Ottosson) | Public domain / MIT reference code | Colour conversion formulas in the checker |
| WCAG 2.2 | W3C document licence | Contrast formula and success criteria |

## Import Safety

No third-party skill text, hooks, scripts or `allowed-tools` entries are
imported. `scripts/check_palette.py` is written for this repository and uses
the Python standard library only. The import-safety test asserts that no
file in this skill contains a fetch instruction or a URL outside a
`## Sources` section or a provenance header.

## Sources

- https://github.com/axismaps/colorbrewer
- https://github.com/Financial-Times/chart-doctor
- https://github.com/vega/vega-lite
- https://github.com/observablehq/plot
