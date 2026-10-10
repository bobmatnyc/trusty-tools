Breaking
- `memory_core::palace::DrawerType` is now `#[non_exhaustive]`. A crate outside trusty-common that matches on it needs a wildcard arm; no crate in this workspace does (#9144).
