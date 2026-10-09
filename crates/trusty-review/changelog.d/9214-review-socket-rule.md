Changed
- The default trusty-search socket comes from `search_rpc::search_socket()` alone, which now applies the daemon's `TRUSTY_DATA_DIR` rule itself; the local copy of that rule is gone. A relative `TRUSTY_DATA_DIR` still fails closed, and the reason now reads "must be an absolute path" (#9214).
