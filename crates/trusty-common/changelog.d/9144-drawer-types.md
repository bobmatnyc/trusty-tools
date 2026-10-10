Added
- `memory_core::palace::DrawerType` has five new variants: `Ruling`, `Decision`, `Status`, `Turn` and `Reference`. They follow `Task`, so every existing postcard index and stored `drawer_type` tag decodes as before. None of them gets a default TTL or dream-cycle protection (#9144).
- `DrawerType::parse_write_type` parses a type a caller names. It accepts exactly `Ruling`, `Decision`, `Status`, `Turn` and `Reference`, in any ASCII case, and returns the new `ParseDrawerTypeError` for anything else. The lenient `DrawerType::from_tag` still decodes an unrecognised stored tag to `Unknown` (#9144).
- `DrawerType` is now `#[non_exhaustive]`. A crate outside trusty-common that matches on it needs a wildcard arm; no crate in this workspace does (#9144).
