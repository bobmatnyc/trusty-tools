Fixed
- Auto-resume of a session that a project decommission stopped now reloads the project's parked search index, as `tm session resume` does. A failed reload is logged as a warning naming the parked index, and the session still starts.
