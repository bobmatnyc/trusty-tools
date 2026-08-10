Added

- `GET /indexes/{id}/complexity_distribution` and the matching `complexity_distribution` MCP tool return the full A-F cyclomatic-complexity histogram over an index, with the counted total, in a payload bounded at five rows regardless of corpus size (#5320).
