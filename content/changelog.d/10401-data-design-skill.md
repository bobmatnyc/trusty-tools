Added
- `data-design` skill (0.1.0): a decision table from data type and question to chart form, encoding, colour and interaction, nine hard rules, and progressive references for chart selection, colour, encoding and labels, hierarchy, time/flow/network/geo, empty and missing data, and accessibility.
- `data-design` ships `scripts/check_palette.py`, a stdlib-only offline palette checker (CVD simulation, OKLCH lightness band, WCAG contrast), and vendored ColorBrewer hex subsets (Apache-2.0) with provenance and the retained notice.
- `data-designer` agent (0.1.0): extends `base-agent`, has no web tools, and returns a design spec for an engineer; chart data never leaves the host.
