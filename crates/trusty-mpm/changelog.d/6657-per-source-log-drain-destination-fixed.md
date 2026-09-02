Fixed

- A per-source destination that cannot be reached is skipped for that tick and
  never retried against the section default. Falling back would ship a project's
  logs to the wrong AWS account, which is the requirement the override exists to
  satisfy. The tick reports `Failed`, the failing destination is named in the
  doctor row, and every other destination still drains.
