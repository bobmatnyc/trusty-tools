Fixed

- The ADR-0044 source-write guard no longer refuses a source write into a git
  checkout rooted under the session scratchpad, so a disposable `git clone --local`
  made for a revert experiment or a probe file is writable again. The exemption is
  decided on the checkout root, never on the write target, so a `scratchpad`
  directory inside a real checkout exempts nothing; a scratchpad root that cannot be
  determined — no `scratchpad` path component under a system temp root — leaves the
  refusal exactly as it was
  ([#7778](https://github.com/bobmatnyc/trusty-tools/issues/7778))
