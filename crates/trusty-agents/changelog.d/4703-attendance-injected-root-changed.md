Changed

- `slack::handlers::post_message` refuses to issue a request when no bot token
  is configured. `chat.postMessage` without a bearer token always answers
  `not_authed`, and the client it builds carries no timeout — so on a network
  that blackholes rather than refuses, a doomed request does not fail, it hangs,
  holding whichever handler called it.
