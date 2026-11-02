Fixed
- An HNSW operation abandoned at its budget is now counted before the store's breaker trips, so any reader that sees the breaker tripped also sees `abandoned_ops` at least 1. Before, a health read or test could catch the tripped breaker with a count of 0 (#10707).
