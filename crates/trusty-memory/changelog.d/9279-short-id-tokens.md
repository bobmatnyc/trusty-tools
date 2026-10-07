Fixed
- The rulings rank floor keeps two-character query terms, so "ruling e1" lifts only a ruling that mentions E1. Before, `e1` was dropped and every ruling that says "ruling" answered the query. Common two-letter words (`am`, `go`, `id`, `vs`, `hi`, `oh`, `is`, `to` and others) stay stop words (#9279).
