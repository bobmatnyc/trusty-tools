Fixed

- `slack_create_canvas` / `slack_canvas_create` against a channel that already
  has a channel canvas surfaced a bare `channel_canvas_already_exists` — the
  caller knew a canvas existed but not which one, a dead end for the normal
  usage pattern of repeat delivery to the same channel. The error now follows
  up with a `conversations.info` lookup and names the existing canvas's file
  id and the fix (`channel_canvas_already_exists (from
  conversations.canvases.create) — channel C1 already has canvas F456; use
  slack_canvas_push to update it`). The lookup is a courtesy, not a
  requirement: if it fails (missing `channels:read`/`groups:`/`im:`/`mpim:read`,
  a transport error, or no canvas on the response) the error degrades to the
  original bare slug rather than masking it (Refs
  [#5161](https://github.com/bobmatnyc/trusty-tools/issues/5161))
