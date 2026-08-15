Fixed
- Tests no longer inherit the machine's real `.env.local` credentials.
  `credentials::redact`'s `resolved_secret_values` test fired the process-wide
  `.env.local` loader while holding no lock, republishing a real
  `OPENROUTER_API_KEY` mid-assertion in `credentials::resolver`'s tests; it now
  joins the `dotenv_credential_env` serial group. The `memory_core::dream`
  dedup, prune, stats and recall tests built on `DreamConfig::default()`, whose
  empty key resolves against that same environment — with one present they
  built a live OpenRouter backend and issued billed LLM calls whose merge
  actions moved the drawer counts they assert on, which is why
  `dream_cycle_merges_duplicates` and
  `dream_cycle_compression_ratio_nonzero_after_dedup` failed intermittently.
  Those tests now disable the semantic phase explicitly and read no credential
  environment variable (#3451).
