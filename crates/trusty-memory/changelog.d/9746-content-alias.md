Fixed
- `memory_remember` accepts `content` as an alias of `text`, and `memory_note` accepts `text` as an alias of `content`. A call that sends both names with different values fails and writes nothing; a call that sends neither gets an error naming both. Both tool schemas list the alias as an optional property (#9746).
