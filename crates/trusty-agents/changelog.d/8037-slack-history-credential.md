Fixed

- `GET /api/agents/{name}/channels/{id}/messages` authenticated its Slack
  history read with the process default token (`BaseClient::new()`) while a send
  on the SAME binding authenticated with the binding's `credential_ref` — so an
  assistant bound to a second workspace read one conversation and wrote to
  another. The read moved into `SlackAdapter::read` and now resolves the same
  credential the send does. Found while moving the last provider arm out of the
  HTTP layer for the stub provider
  ([#8037](https://github.com/bobmatnyc/trusty-tools/issues/8037)).
