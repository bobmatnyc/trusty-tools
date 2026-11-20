Changed
- The per-org installation id variable hard-coded to one org name is deprecated. Set `GITHUB_INSTALLATION_ID_DEFAULT_ORG` and `GITHUB_INSTALLATION_DEFAULT_ORG` (the org name) instead. The old variable is still read as a fallback and logs a warning naming the new variables.
