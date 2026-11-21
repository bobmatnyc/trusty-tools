Fixed
- `trusty-search doctor` checks the model cache at the path the live embedder uses (`~/.cache/fastembed`, or `FASTEMBED_CACHE_DIR`) and for the model it loads, so a cached model no longer reads as missing (#10590).
- `trusty-search doctor` reads the Claude Code MCP registration from `~/.claude.json`, `CLAUDE_CONFIG_DIR` and the trusty-mpm managed tier, so a managed-profile registration no longer reads as absent. A registration missing from every tier still warns (#10590).
