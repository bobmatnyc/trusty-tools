Changed

- The Intent Ledger session record limit is 8192 B, up from 2048 B, so a usage hold and a merge hold at full length fit beside the rest of the record. `tm fleet init` and the shipped `datg.py` use the new limit; a record over it is still refused whole, with the largest fields named.
