Changed

- The ledger session record limit (`SESSION_MAX`) is 8192 B, up from 2048 B, matching `datg.py` and `tm fleet init`. A record over it is still refused.
