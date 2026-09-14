Added

- Session history is published to per-session, append-only git refs
  (`refs/tm/sessions/<user-id>/<session-key>`) instead of living only in the
  gitignored local cache ([ADR-0062](../../../docs/adr/0062-session-history-as-per-session-git-refs.md),
  refs [#7830](https://github.com/bobmatnyc/trusty-tools/issues/7830))
  - `session_context_pause` appends one orphan-chain commit per pause and
    lease-pushes it with `--force-with-lease`; a stale lease is reported, never
    retried with `--force`
  - `session_context_catchup` hydrates the local `.trusty-mpm/sessions/` cache
    from those refs before anything reads it, so a fresh clone resolves its
    snapshot through the unchanged read path
  - the publish is fail-open: a ref or push failure never fails the pause, and
    the response carries `ref_name`, `ref_published` and `ref_error`
  - a pre-push credential scan refuses to publish a snapshot carrying
    provider-key-shaped content; the local cache keeps it
  - new `[session_refs] enabled` config key, defaulting to `true`; `false`
    restores the previous behaviour exactly
  - retention of old session refs and the cross-user sharing model are out of
    scope here — both are deferred by ADR-0062 decision 8
  - known follow-up: a lost lease is classified from git's human-readable push
    stderr rather than from `git push --porcelain`, so a future git wording
    change could reclassify it as a plain transport failure
