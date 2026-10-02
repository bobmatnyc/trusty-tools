Fixed

- In-project cold start (`tm <owner>/<repo>`) no longer refuses an existing
  checkout whose origin uses an `~/.ssh/config` host alias. An origin such as
  `git@github-duetto:duettoresearch/APEX.git`, with `Host github-duetto`
  mapped to `HostName github.com`, now matches
  `https://github.com/duettoresearch/apex` instead of failing with
  `RemoteMismatch`. A host with no alias entry, or an ssh config that cannot
  be read, compares exactly as before
  ([#9089](https://github.com/bobmatnyc/trusty-tools/issues/9089)).
