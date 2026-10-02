Added
- The 43 agent sources (including the five compose-only `BASE-*.md`) now live
  under `content/agents/`, and the four harness-understanding docs under
  `content/instructions/harness_understanding/`, moved from
  `crates/trusty-agents-common/src/assets/` (#9011).
- Every agent declares `metadata: {version: "0.1.0"}`; `content/manifest.toml`
  lists all 43 as members under bundle version 0.2.0 (#9011).
