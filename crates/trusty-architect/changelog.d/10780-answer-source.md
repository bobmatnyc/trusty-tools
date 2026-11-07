Added
- The ledger writer reads and keeps an answer's `source`, and writes the Architect form (`update` op `source`: `{by: architect, basis}`) in byte parity with the Architect's `datg.py`. It refuses `{by: owner, …}`: only `datg.py`, which checks the quote against the owner-turn record, writes an owner source (#10780).
