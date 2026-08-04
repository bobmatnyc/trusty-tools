Added

- `agents::quarantine` — moves an untracked project-tier agent file that SHADOWS a bundled agent name out of the way, reversibly (closes [#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448))
  - a file moves only when all FOUR gates agree: it resolves to a bundled name, the ownership ledger does not record it as the operator's, git does not track it, and its frontmatter is trusty-mpm's own composer output. Each gate is independently fail-closed
  - `agents::vcs_claim` — gate 3. A repository that COMMITS a project-tier agent is declaring it, so `git ls-files` stands in for the `Origin::Project` declaration [#4443](https://github.com/bobmatnyc/trusty-tools/issues/4443) was going to provide. Three states, never a bool: "no repository" and "git could not be asked" have opposite safe answers
  - `agents::agent_schema` — gate 4. claude-mpm, a separate live project, deploys into the same `.claude/agents` convention reusing trusty-mpm's exact filenames under a different schema; classification is by frontmatter key set, never by filename or file size
  - move-with-backup only. A verified byte-identical copy is taken before the original is renamed to an inert `.md.disabled` sibling. No code path calls `remove_file` or `remove_dir`, pinned by `never_deletes_on_any_path`
  - every examined file lands in exactly one of the report's moved / skipped / failed lists, and the receipt is rendered from that report — so a run that fails part way still records what moved and what did not
  - the receipt's restore command POSIX-single-quotes every path, so a filename carrying `$(…)` or backticks cannot execute when pasted
