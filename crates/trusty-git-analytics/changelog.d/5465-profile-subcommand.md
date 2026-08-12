Added

- `tga profile <contributor>` (closes #5465, part of epic #5468). One command runs the whole pipeline: identity resolution, period batches, diff sampling, per-period review through `trusty_common::inference`, cross-period synthesis, and a JSON/Markdown report. `--dry-run` produces a deterministic model-free profile; `--github-issue --github-repo <owner/repo>` publishes the report to a per-contributor GitHub issue thread. This is the tga-side equivalent of trusty-review's `profile` subcommand, which #5466 removes.

  The command is the first caller of `PeriodReviewer::review_period`, and it honours what #5464 built: a period whose provider call failed is reported as SKIPPED, never as a period with no findings. `profile::PeriodRunSummary` folds each review in, counts the two outcomes apart, and prints a coverage line; when any period was skipped the report itself carries a Coverage section naming them and stating they are not evidence of clean work. Without that, an outage across twelve quarters renders as twelve clean quarters and the trajectory covers a sample the reader cannot see is smaller.

- `profile::Synthesizer` runs the narrative pass over the same shared-inference adapter, so the profile's prose no longer needs trusty-review. A provider failure writes the deterministic fallback narrative AND returns the error, so a caller can say the narrative is a fallback rather than presenting it as the model's.

- `profile::reporter_github` publishes a rendered profile to a GitHub issue thread — `GithubIssueConfig`, `issue_title`, and `upsert_profile_issue`. The title embeds the canonical email, which is what the next run searches for, so a contributor accumulates one thread rather than one issue per run.

- `Reporter::render` exposes the Markdown the reporter writes, and `Reporter::with_coverage_note` splices the skipped-period note above the first section. The GitHub issue body is that exact text, so the file on disk and the posted comment cannot drift.
