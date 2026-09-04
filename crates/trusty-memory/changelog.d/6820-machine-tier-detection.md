Added

- **The daemon now detects what machine it is on.** `AppState` gained a public `machine: trusty_common::machine_tier::MachineBudget` — the cgroup-clamped RAM reading, the tier band, and the two proportional soft limits, resolved through the same shared module trusty-search reads. Before this trusty-memory detected nothing: `max_open_palaces` stayed at `DEFAULT_MAX_OPEN_PALACES` (64) whether the host had 12 GB or 128 GB, and nothing in the log said which machine the daemon had sized itself for ([#6820](https://github.com/bobmatnyc/trusty-tools/issues/6820))
  - `serve --foreground` logs the tier and both limits at startup, and warns once when the host is below the documented 16 GB minimum
  - **No cap is retuned here.** #6820 is scoped to detect, expose, and document; [#6823](https://github.com/bobmatnyc/trusty-tools/issues/6823) is what makes `TRUSTY_MEMORY_MAX_OPEN_PALACES` RAM-proportional by reading this field
  - Adding a public field to a struct with public fields and no `#[non_exhaustive]` breaks a downstream struct literal, which is why this is a MINOR bump
