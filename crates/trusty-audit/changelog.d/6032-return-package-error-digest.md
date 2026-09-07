Added

- The return package now carries `errors/digest.json`, a machine-readable record
  of everything a run recorded as gone wrong: a repository whose `tga audit`
  child failed, a dimension a repository could not assess, a board the sweep
  could not collect, a target that never cloned, and a config key this version
  does not act on. Each entry names the stage, whether the run stopped or carried
  on, the repository it concerns, when it was recorded, and the message. It is
  written on every run, empty `entries` array and all, so an empty digest states
  that the collector ran rather than leaving the recipient to guess. Every
  message is scrubbed of the engagement's configured secrets and the `gh`-derived
  token before it is written, and the rendered document then goes through the
  same credential refusal every other generated member does. New public items
  `package::DIGEST_ENTRY` and `run::RepoRun::finished_at`; no existing member,
  signature or struct shape changed (issue #6032).
