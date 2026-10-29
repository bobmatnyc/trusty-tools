Added

- `EmbedderClient::last_reported_window`, read by `StdioEmbedderClient` from the optional `result.window` field of the first success frame that carries it; an earlier error or orphan frame no longer blocks it (#9284).
