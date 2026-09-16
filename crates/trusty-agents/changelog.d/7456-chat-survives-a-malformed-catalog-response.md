Fixed
- The web UI no longer blanks the chat when `GET /api/models` or `GET /api/workstreams` answers 200 with a body of the wrong shape. The model catalog is rejected unless it carries `providers` and `local`, so the picker falls back to "Default" only, and a non-array workstreams body falls back to an empty list.
