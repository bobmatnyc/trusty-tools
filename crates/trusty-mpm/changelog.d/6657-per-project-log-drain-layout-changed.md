Changed

- The log drain writes to `<owner>/<project>/<crate>/<file>` beneath the
  destination prefix, replacing `<github_id>/<session_id>/logs/…` (#6657). Each
  source resolves its owner and project from its own `owner:`/`project:`, else
  the git `origin` of its `root` (git walks up, so a log directory inside a
  checkout resolves to that checkout), else the new section-level
  `owner:`/`project:`. A source that resolves none of the three is a hard config
  error naming the source and its root — the drain never uploads under a
  placeholder key. Sources are grouped by destination AND project, so one bucket
  holding three projects runs three passes.
- `log_drain.github_id` and `log_drain.session_id` are gone with the key
  segments they filled, along with the `gh api user` probe and the persisted
  per-install session id. Objects already uploaded under the old layout stay
  where they are; see `docs/reference/log-drain.md`.
