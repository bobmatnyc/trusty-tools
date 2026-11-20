Added
- `slack-mcp` gains `slack_canvas_access_set`, which calls `canvases.access.set` to grant users or regular channels `read` or `write` access on a bot-owned canvas, so a canvas shared as a link can be edited. It refuses `owner`, DM channel ids, and calls that name both or neither of `user_ids` and `channel_ids`. Needs the `canvases:write` scope the canvas tools already use (#10890).
