Changed
- A user-initiated forget (`memory_forget` over MCP, or the drawer delete on the service API) now logs one `info` line naming the palace, the drawer id and the caller (`mcp`, `http`) when a drawer is actually removed. A forget of an id that is not stored logs nothing (#8729).
