Breaking
- `core::classifier::QueryIntent` gains a `Keyword` variant. Code that matches the enum exhaustively must add an arm; giving it the `Unknown` arm keeps the previous behaviour (#9027).
