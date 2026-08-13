Fixed

- A repository git cannot read is no longer reported as one with no remote
  (refs [#4734](https://github.com/bobmatnyc/trusty-tools/issues/4734)).
  `inproject::get_origin_url` returned a bare `Option`, so "this repo has no
  `origin` remote" and "git could not answer at all" were the same value, and
  every caller read it as the former. `tm launch` and the guided default told
  the operator a checkout they could not read had no GitHub remote, and sent
  them to `tm connect` to fix a repo that is fine. It now returns
  `Result<Option<String>, String>`: a git-exec failure, or a fatal exit such as
  128 for an unreadable `.git/config`, a dangling gitdir pointer, or a
  `safe.directory` ownership refusal, is `Err`, while git-config's exit 1 — its
  "the key is not set" answer, and what both a plain non-git directory and a
  repo without an `origin` produce — stays `Ok(None)` and still falls through
  by design.
- A `.git` git cannot open no longer answers with a DIFFERENT repository's
  remote. Discovery walks past an unreadable `.git`, so a `.git` directory at
  mode 000 exited 1 with empty stderr (indistinguishable from "no origin"), and
  the same directory nested inside another repo exited 0 with the PARENT repo's
  origin — which would have the daemon provision a managed clone of the wrong
  repository. When `path/.git` exists, git's own `rev-parse --show-toplevel`
  must now name that path back.
- A managed checkout whose remote git cannot read is no longer reported as
  having no remote, which told the operator to move or remove a directory that
  may be fine.
