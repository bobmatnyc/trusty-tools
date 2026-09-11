Added

- `McpConfigFile::parse` and `McpConfigFile::render` expose the parse and render halves of `load`/`save`, so a consumer doing a locked read-modify-write can decide from the bytes it already read instead of reading the path a second time outside its lock (#7454). `load` and `save` are unchanged and still the right entry points for a plain read or write.
