Changed
- An exclude glob that does not parse is now rejected where it enters: `POST /indexes` and `PATCH /indexes/{id}/config` answer `400 invalid_exclude_glob`, and a `trusty-search.yaml` holding one fails to load. A persisted invalid glob still excludes every path at runtime as a backstop, and its error is logged once per pattern instead of once per path (#8922).
