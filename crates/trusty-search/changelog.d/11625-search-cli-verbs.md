Added
- `trusty-search similar`, `call-chain`, `grep` (`--all` for every index) and `typeahead`: CLI verbs for the MCP `search_similar`, `get_call_chain`, `grep` and `typeahead` tools. Each sends the same socket method and params as its tool, and `--json` prints the method result (#11625).
- `trusty-search list --details` sends `details: true`, so `list --details --json` matches the MCP `list_indexes` tool. `query --indexes "*"` is documented as the CLI form of `search_all` (#11625).
