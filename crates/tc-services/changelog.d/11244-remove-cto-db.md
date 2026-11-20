Breaking
- Removed the `cto_db` module (`CtoDbService`, `cto_db_services`, `CTO_DB_TOOL_NAMES`, `CtoDbOutcome`) and the dependency on the `trusty-cto-db` crate, which is deleted from the workspace. The CTO database queries now live in an internal project; the `granola` and `gworkspace` modules are unchanged (#11244).
