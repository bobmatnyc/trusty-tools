Fixed
- The rulings rank floor keeps two-character query terms, so "ruling e1" lifts only a ruling that mentions E1. Before, `e1` was dropped and every ruling that says "ruling" answered the query (#9279).
