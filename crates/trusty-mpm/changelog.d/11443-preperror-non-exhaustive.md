Changed
- `session_launch::PrepError` is now `#[non_exhaustive]`, so a later variant no longer breaks a downstream exhaustive `match`. Code outside trusty-mpm that matches on `PrepError` needs a wildcard arm; this is a one-time public-API break, scheduled for 1.15.0 (#11443).
