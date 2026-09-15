Fixed
- `tagent system status` and its JSON report now list every declared binding that does not resolve, not only stores (#7903). The new `unresolved_bindings` entries cover a missing `[tools].search_indexes` id, a listener binding naming no harness listener, an `[mcp]` override naming no server or declaring an unusable one, and a default search slot that fails to resolve.
- The `[mcp] disabled` entry naming no global server now reads as plain English, naming both the disabled entry and the missing server, instead of the earlier "which no global server is named" fragment.
