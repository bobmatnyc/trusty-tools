Security
- `inference::types::SecretString`'s `Debug` and `Display` no longer disclose
  the first four characters of the wrapped credential, nor its byte length.
  Both now write the fixed constant `SecretString(<redacted>)`, computed from
  no part of the value, which is what DOC-45 `C-8.2` requires. Anything that
  derives `Debug` and holds a `SecretString` — `ResolvedProvider`, whose own
  doc comment promised `Debug` was safe to log — printed a live API key's head
  before this. Two property tests pin the guarantee: the rendering never varies
  with the value, and it contains no substring of it (#4632).
