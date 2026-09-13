Added

- New `self_improvement` module: harness-agnostic extraction of the two BASE-AGENT closing-block sections — `extract_improvement_recommendations` returns the structured Symptom/Cause/Change/Evidence findings under `## Improvement recommendations`, and `extract_prompt_feedback` mirrors `trusty-mpm`'s `## Prompt feedback` extractor shape. Both tolerate a missing or malformed block and `##`/`###` heading depth, and never panic on arbitrary input. Slice 1 of `docs/specs/self-improvement-loop.md` (#7735).
