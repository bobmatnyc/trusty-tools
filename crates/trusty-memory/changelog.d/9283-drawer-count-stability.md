Added
- `doctor` checks drawer-count stability (#9283). The daemon records one drawer count per palace per UTC day in `<data_root>/drawer_counts.jsonl` (90 days kept; a palace it cannot read is recorded as unavailable, never zero). The check turns red when a palace's newest drop exceeds the deletions its `maintenance_deletions.jsonl` journals, and warns when a palace disappears.
- `trusty-memory doctor --drawer-report [--days 7] [--json]` prints each palace's daily counts, net delta, journaled deletions by reason and unexplained drops, with a `N/7 clean` footer; it exits 1 on any unexplained drop (#9283).
- `trusty-memory doctor --ack-drop <palace> --count N --reason TEXT [--ack-day YYYY-MM-DD]` records an operator acknowledgement that explains a drop the journal cannot (#9283).
