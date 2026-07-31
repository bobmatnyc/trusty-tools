---
name: tm-slack-canvas-delivery
description: Deliver a document to the user as a Slack canvas — create, bind to a destination, post the link, and verify — never treat canvas creation alone as delivery
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [slack, canvas, delivery, pm-required]
effort: low
---

# tm Slack Canvas Delivery Protocol

## When to Use

Delivering any document to the user via Slack — a report, spec, digest,
runbook, or postmortem — as a Slack canvas.

## The Completion Rule

**Creating a canvas is not delivery.** A message containing the link, sent
into a destination the user actually watches, is delivery. Never mark the
task done on the create call's response alone — that response only proves
the canvas exists, not that the user has any way to find it. The delivery
receipt is the successful `slack_send_message` response (message ts/link),
not the canvas creation response.

## Step-by-Step Protocol

1. **Resolve the destination first.** Reuse the triggering Slack
   conversation's channel/thread when known. Otherwise resolve the user's ID
   (`slack_search_users`, then DM using the user_id as the channel_id) or the
   named project channel (`slack_search_channels`). Ask once if genuinely
   ambiguous — never invent a channel silently.

2. **Create the canvas.** Two tool surfaces exist:
   - The claude.ai connector —
     `mcp__claude_ai_Slack__slack_create_canvas` — takes `title` and
     `content` only. It has no channel-binding parameter.
   - The native trusty slack-mcp — `slack_canvas_create` — **always** pass
     `channel_id`. This tabs the canvas into the channel, and free-tier
     Slack workspaces hard-fail canvas creation without it
     (`free_teams_cannot_create_non_tabbed_canvases`).

   Prefer the native tool when it's registered (check the tool listing for
   availability). The claude.ai connector is the fallback surface.

3. **Capture the link/id from the creation response.** The native tool
   returns only a `canvas_id`, with no permalink. In that case, rely on the
   channel tab itself ("added a canvas to #<channel>: <title>") as the
   reference, or a `canvas_url` surfaced by a subsequent update/push call.

4. **Deliver.** Call `slack_send_message` into the resolved destination with
   the canvas title and link (or an explicit reference to the channel tab if
   no link exists).

5. **Verify.** Confirm the send call actually succeeded — that response is
   the completion signal, not the canvas creation response.

## Failure Checks Before Declaring Delivered

- Canvas creation actually succeeded — watch for the free-tier
  non-tabbed-canvas error.
- A `slack_send_message` call was actually made this turn, not just
  planned.
- The destination matches where the user watches, not just any channel that
  happened to resolve.

## Fallback When No Canvas Tool Is Available

Never silently drop the deliverable. Post the content as formatted Slack
messages instead (chunked if long), or publish it as an Artifact or another
available channel and hand back that link. Degrade the medium, never drop
the payload.

## Related Skills

- `internal-comms` — writing the digest/report content that gets delivered
- `tm-bug-reporting` — the sibling pattern of an MCP-native tool with a
  documented fallback path
