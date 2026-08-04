Fixed

- `code-review-standards` and the `code-critic` agent (both synced to
  `trusty-code`) no longer let `Promote` act as a filing engine. The default
  is now stated explicitly: a review finding is fixed in the surfacing PR, or
  dropped. `Promote` is reserved for defects that are genuinely separable,
  schedulable work — not every LOW/MEDIUM finding — and the critic never
  files an issue itself or instructs anyone to; it only recommends, and the
  PM or user decides. Prompted by nine issues filed in a single review leg
  against one merged PR, several forwarded straight from an unqualified
  `Promote` disposition
