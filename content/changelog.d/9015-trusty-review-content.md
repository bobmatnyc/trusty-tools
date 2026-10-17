Added
- `instructions/trusty-review/prompts/` (`system_prompt_stock.md`, `system_prompt_coverage_gating.md`): trusty-review's reviewer system prompts, moved out of the `trusty-review` binary byte for byte. They carry no frontmatter, because the files are sent to the model verbatim; under `instructions/` a version is optional, so they are not manifest members (#9015).
