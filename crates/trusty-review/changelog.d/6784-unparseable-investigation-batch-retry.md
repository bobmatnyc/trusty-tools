Fixed

- An investigation batch whose response could not be parsed is retried once
  instead of failing closed on the first attempt, so the files it carried are
  still read. 37 of 59 repositories in one engagement logged
  `unparseable response` on at least one batch, and every such batch was dropped
  with no second call — which is what collapsed investigation coverage on the
  largest repositories (#6784).
- `parse_findings` now decodes three response shapes it used to reject outright:
  a conforming object behind a prose preamble, an untagged ``` fence, and an
  object followed by trailing prose. Any one of them dropped a whole batch
  (#6784).
- A response the provider cut off mid-object without setting `finish_reason` is
  classified as a truncation rather than an unparseable answer, so it reaches the
  concise retry built for exactly that case instead of skipping it (#6784).
