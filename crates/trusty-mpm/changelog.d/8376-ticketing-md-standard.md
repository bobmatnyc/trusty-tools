Added

- `tm-ticketing` (2.1.0) states its taxonomy and behaviour as explicit defaults, names the resolution order a project-root `TICKETING.md` sits at the top of, and carries the canonical skeleton the ticketing agent copies when generating one ([#8376](https://github.com/bobmatnyc/trusty-tools/issues/8376))
  - the defaults now cover the epic/phase title shape and its marker-delimited Tracker section, a per-phase follow-up budget with a severity floor and a due-by window, and a staleness policy whose human decisions are requested per epic as a digest
  - `Refs #N` and `trusty-mpm` as a component label stay fixed at every tier
  - `tm-issues-prune` points at the same policy for the thresholds its Prune phase applies
