Added
- `trusty_memory::engine` re-exports the memory storage engine (`trusty_common::memory_core`) under every feature set, including `--no-default-features`. Each item has the same type through both paths, so a consumer can switch its imports to `trusty_memory::engine` now (#9270).
