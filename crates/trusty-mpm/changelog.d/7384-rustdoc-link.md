Documentation

- Fixed a broken intra-doc link in `run_compress`'s doc comment (`commands/compress.rs`) that pointed at `compress_tool_output_async_with_path`, a name not in scope in this module; it now points at `compress_with_raw_fallback`, the function `run_compress` actually calls (#7384).
