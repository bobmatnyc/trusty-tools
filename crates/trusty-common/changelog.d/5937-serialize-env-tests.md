Fixed
- Tests that write `HOME`, `OLLAMA_HOST` or the memory-core timeout knobs now share one serialization domain with every other writer and reader of the same variable: `workspace_layout` and `memory_core::timeouts` take `data_dir::ENV_LOCK`, the HOME writers share one `serial` key, and the `OLLAMA_HOST` writers share `dotenv_credential_env` (#5937). Test-only.
