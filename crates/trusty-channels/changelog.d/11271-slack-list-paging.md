Fixed
- slack-mcp: `slack_list_channels` pages with `cursor`/`next_cursor` and lists private channels by default, falling back to public channels (and reporting the `types` listed) when the token lacks `groups:read`; channel reads that get `channel_not_found` name the bot user to invite (#11271).
