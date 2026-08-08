Added

- `uds::rpc::send_framed_request` — the shared one-request/one-response,
  newline-framed JSON transport over a hardened Unix socket (ADR-0034 §4,
  #5089 step 3). Dials through `connect_hardened`, so the socket's `0700`
  directory and `0600` mode are verified before a byte is written; caps the
  response at `MAX_FRAME_BYTES` and bounds the whole exchange with a
  caller-supplied timeout. Errors are a `UdsRpcError` variant per failure
  point, and none of them may be read as an acknowledgement. The four existing
  hand-rolled clients (`embedder_client/uds.rs`, `bm25_client.rs`, and
  trusty-agents' `ctrl/socket.rs` and `bus`) migrate onto it in a follow-up
- `webhook_hmac` (feature `webhook-hmac`) — the single GitHub
  `X-Hub-Signature-256` verifier. `verify_github_signature` returns a
  three-state `SignatureVerdict` rather than a `bool`, so "no secret is
  configured" cannot be collapsed into "the signature is wrong" and silently
  become permission to proceed — the shape of trusty-analyze's live fail-open
  (ADR-0034 §3). `sign_github_body` is the matching test-harness helper.
  trusty-analyze's and trusty-review's copies are retired in #5089 step 4
- `webhook_relay` (feature `webhook-relay`) — the console→target relay wire
  contract: `RELAY_METHOD`, the borrowed `RelayFrame` the sender writes, the
  owned `RelayRequest` a receiver reads, `Provenance`, and `RelayResponse`.
  It lives here rather than in trusty-console because step 4's receivers are
  trusty-review and trusty-analyze, which cannot depend on the console — a
  contract held by one half only is two copies waiting to drift.
  `RelayResult::ack` is `#[serde(default)]` and `RelayResponse::is_ack` is the
  only predicate a deletion path may consult, so a receiver that answers with
  an empty result object has not acknowledged
