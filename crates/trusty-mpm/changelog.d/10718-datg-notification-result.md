Fixed
- Bundled `datg.py` returns exit 3 when a ledger update or batch is saved but an escalation notification cannot be confirmed. Partial failures report each sender outcome and warn against replaying committed writes; successful and ordinary writes keep their existing stdout and exit 0 behavior (#10718).
