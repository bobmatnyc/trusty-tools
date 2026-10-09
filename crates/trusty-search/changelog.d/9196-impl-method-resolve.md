Fixed
- `get_call_chain` and other entry-point lookups now resolve `<path>::<method>` to a method defined inside an `impl Type` or `impl Trait for Type` block. The path suffix is matched against the defining file, so `src/a.rs::m` finds `src/a.rs::Foo::m` instead of answering "entry point not found".
