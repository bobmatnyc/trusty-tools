Added

- `tm doctor` gained a `transcript_saving` check (24 checks, was 23) that fails
  when any `tm` launch line would leave transcript saving disabled — the defect
  it guards was silent until Claude Code began warning about it. It reads the
  scrub set out of the real launch commands rather than restating a constant,
  names the offending builder on failure, and fails in both directions:
  under-scrub costs the session its transcripts, over-scrub costs it the agent
  roster ([#4451](https://github.com/bobmatnyc/trusty-tools/issues/4451)) or the
  OAuth token ([#2246](https://github.com/bobmatnyc/trusty-tools/issues/2246)).
