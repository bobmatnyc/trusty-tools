Fixed
- `service::SearchClient` no longer routes its loopback daemon calls through an
  exported `HTTP_PROXY`. It builds through
  `trusty_common::http_client::loopback_client_builder` (#4392).
