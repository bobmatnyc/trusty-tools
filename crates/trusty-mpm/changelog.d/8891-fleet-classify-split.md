Changed
- `tm fleet init` now also writes `scripts/fleet-classify.py` into the Architect project. The fleet poller, `scripts/fleet-poll.py`, was split to get under the 500-line cap, and it loads this new sibling file by path. The poller behaves as before (#8891).
