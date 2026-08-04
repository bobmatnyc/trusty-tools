Changed

- **`slack::handlers::post_message` now fails fast when no bot token is
  configured, instead of issuing the request.** `chat.postMessage` without a
  bearer token always answers `not_authed`, and the HTTP client this function
  builds carries **no timeout** — so on a network that blackholes rather than
  refuses, a doomed request does not fail, it hangs, holding whichever Slack
  handler called it. The refusal is returned as an **error**, not a silent
  `Ok(())`: `handle_message` mirrors its reply to the GUI only when the send
  succeeded, so swallowing this would surface a reply the Slack channel never
  received. A configured gateway is unaffected — `run_slack_bot` reads
  `SLACK_BOT_TOKEN` at startup and refuses to run without it.
