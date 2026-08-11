Fixed

- `slack_create_canvas` and `slack_canvas_create` given a `channel_id` now create
  a **channel** canvas via `conversations.canvases.create` instead of a
  standalone one via `canvases.create`. Both tools only ever called
  `canvases.create`, which creates a canvas owned by the acting identity — with a
  bot token, the bot owned it, and the human who asked for the document got it
  view-only and had to duplicate it to edit. Passing `channel_id` looked like it
  should already fix that, but on `canvases.create` that argument only tabs the
  canvas into the channel cosmetically and confers no edit rights. A channel
  canvas ties access to channel membership instead, so every member can edit it
  with no separate share step. Same bot token and same `canvases:write` scope —
  no new OAuth grant. Without `channel_id` both tools keep calling
  `canvases.create` unchanged. Both endpoints return the created id under the
  same top-level `canvas_id`, and one shaping function reads it, so the MCP
  response is identical either way. Slack rejects a second channel canvas for the
  same channel with `channel_canvas_already_exists` — edit the existing canvas
  rather than recreating it. Canvas-create errors now name the endpoint they came
  from (`<slug> (from <method>)`) (closes
  [#5155](https://github.com/bobmatnyc/trusty-tools/issues/5155))
