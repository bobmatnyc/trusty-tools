Fixed
- `memory_remember` accepts `content` as an alias of `text`, and `memory_note` accepts `text` as an alias of `content`. A call that sends both names with different values fails and writes nothing; a call that sends neither gets an error naming both. Both tool schemas list the alias as an optional property (#9746).
- The `memory_remember` and `memory_note` schemas no longer mark `text` or `content` as required, so an alias-only call passes a client that validates against the schema. Both descriptions state that one of `text` or `content` is required, and the handler enforces it (#9746).
