Fixed

- **Credentials resolve through a lookup the caller supplies, not the process
  environment (#6405)** — 42 `set_var`/`remove_var` sites across ten files
  mutated the environment while 50+ reader threads, plus reqwest and rustls,
  called `getenv` under `cargo test -p tga`. `setenv` can reallocate the
  environment array mid-`getenv`, the likely mechanism behind the #2613 SIGSEGV.
  Every JIRA, GitHub Issues, Linear, Shortcut, Confluence and Datadog fetch
  helper now has a `_with_creds` variant carrying a `CredentialSource`, and
  `ExternalSourceResolver` threads one through `resolve` and `warm_cache`; the
  LLM tier's `from_provider` and `from_llm_config` take one too. Every existing
  public function keeps its signature and delegates with the environment-backed
  lookup, so no caller changes. `marker_file_path` and `resolve_db_path` take
  their variable's value as a parameter, retiring a module-local mutex and three
  `#[serial]` attributes. One site remains, in `batch_reviewer_tests`, because
  the env tier it exercises lives in `trusty_common` and takes no lookup — it
  stays `#[serial]` with that reason recorded at the call site.
