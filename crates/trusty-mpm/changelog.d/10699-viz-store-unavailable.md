Fixed
- Architect dashboard: `/api/viz` answers 503 when the store is locked by a writer, instead of 200 with empty sessions and goals; other store errors are no longer turned into an empty view.
