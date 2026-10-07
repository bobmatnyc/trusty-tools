Fixed
- The rulings rank floor keeps two-character query terms, so "ruling e1" lifts only a ruling that mentions E1. Before, `e1` was dropped and every ruling that says "ruling" answered the query. Common two-letter words (`am`, `go`, `id`, `vs`, `hi`, `oh`, `is`, `to` and others) stay stop words (#9279).
- `memory_recall` and the other recall surfaces rank a drawer first when the query names a rare id with a digit, such as `e1`, `v2` or `#42`. A two-letter word without a digit, such as `pm` or `pr`, does not change the ranking (#9279).
