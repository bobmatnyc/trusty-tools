Added

- Public modules `trusty_review::pipeline::pr_index` (`PrIndex`, `IndexPin`,
  `resolve_pr_index`) and `trusty_review::config::repo_index`
  (`RepoIndexError`, `PinOrigin`), and `trusty_review::pipeline::caller_preamble`,
  which splits a `# Context:` preamble off a diff and caps caller-context
  fields (`cap_caller_context`).
