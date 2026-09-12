Fixed

- `tm hook --pm-guard` no longer refuses a git REF argument or a human-readable
  text payload for naming a secret-bearing file. `git checkout -b
  feat/7526-secrets-manager-agent`, `git switch -c feat/7527-tm-secrets-skill`,
  `git branch [-m] <ref>`, `git push origin <ref>` and a `gh issue comment
  --body`/`--title`/`--message`/`--note` whose prose names a `.env` file all
  allow. Both positions are identified by POSITION, never by verb: a ref
  argument reads as a ref only through the same `reads_as_a_branch_name`
  predicate the word-list rule uses, so `git checkout -b .env`,
  `git push origin id_rsa` and `git checkout -b config/credentials` still deny,
  and a payload is matched by exact long-flag spelling, so
  `gh issue comment 1 --body-file .env` and `git commit -F .env` still deny
  (#7498).
- A `git checkout`/`git switch` new-branch flag written AFTER the `--`
  separator no longer opens a ref window over the real pathspec, so
  `git checkout main -- -b docs/api-secrets` denies exactly as
  `git checkout main -- docs/api-secrets` always did. The subcommand's index
  now comes from the shared `git_argv_at_subcommand` parser rather than a
  second argv walk (#7498).
