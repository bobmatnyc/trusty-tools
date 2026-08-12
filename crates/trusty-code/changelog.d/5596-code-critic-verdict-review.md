Changed

- `code-critic`'s embedded copy is reconciled with trusty-mpm's upstream
  "post the verdict as a COMMENT-type GitHub review" change, reworded to fit
  its read-only `tools:` restriction: it hands the caller the exact
  `gh pr review --comment` command instead of running it directly, since this
  agent has no `bash`/`gh` tool. Re-pinned in `scripts/agent-asset-pins.tsv`
  per the E4 staleness guard (`scripts/check_agent_assets.sh`).
