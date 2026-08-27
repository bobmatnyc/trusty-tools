Fixed

- A repository's output directory carries an `audit-complete.toml` marker,
  written last and only for a repository the run got all the way through, and a
  re-run re-collects any recorded output that does not have one. `manifest.toml`
  was the only evidence a directory held, and it is written by the `tga audit`
  child before the grounding pass and the inference stamp edit it — so a run
  killed in between left real collection data and a report that was never
  finished, which `verify_output` accepts and a re-run carried over as complete.
  Completion was recorded only in `state/run-progress.toml`, one document for the
  whole sweep, so nothing about the directory itself said which it was; the
  marker makes that answer travel with the data, for a directory copied,
  inspected or re-rendered on its own. The re-collection is announced with its
  reason rather than being silent, and a record written before the marker existed
  is re-collected once — the same direction `RunProgress::complete` takes for the
  same reason (#6141)
