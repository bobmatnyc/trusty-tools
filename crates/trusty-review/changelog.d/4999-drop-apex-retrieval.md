Removed

- APEX/KB retrieval no longer runs during a review. The `apex_context` module,
  the `apex_index` / `apex_path_prefixes` config surface (env vars
  `TRUSTY_SEARCH_APEX_INDEX` / `TRUSTY_REVIEW_APEX_PATH_PREFIXES`, now ignored),
  the `[apex: …]` inline-citation form, and the `ReviewContext::apex_results`
  field are gone. APEX context was cited 0 times across 69 audited findings at
  ~0.001 relevance, so the owner ruling dropped the retrieval step rather than
  forcing citation (#4999). GitHub and JIRA/Confluence context retrieval are
  unchanged.
