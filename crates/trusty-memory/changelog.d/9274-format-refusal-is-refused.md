Changed
- Opening a palace by id over MCP now reports a palace-format refusal (for example a palace written by a newer release) as a refused error whose message names the variant, such as `FormatTooNew`, instead of an internal error (#9274).
