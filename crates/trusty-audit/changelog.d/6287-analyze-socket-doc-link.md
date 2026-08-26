Documentation

- `Tools::pinned`'s doc links `daemons::analyze_socket` instead of the
  `daemons::analyze_base_url` that #6287 removed, and says the resolved pair is
  a search URL plus an analyze socket path. The broken link failed Gate 1 of
  the pre-publish workflow.
