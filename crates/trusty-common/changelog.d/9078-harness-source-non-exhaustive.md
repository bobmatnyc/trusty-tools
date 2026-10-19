Breaking
- `control_bus::HarnessSource` is `#[non_exhaustive]`: a `match` on it outside trusty-common needs a wildcard arm. Later producers can then be added without another breaking release (#9078).
