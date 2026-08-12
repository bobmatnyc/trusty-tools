Added
- `download::http_client()` is public. `download::pinned::install_pinned_set` takes a `&reqwest::Client`, and its out-of-crate caller had no way to obtain one without answering "how is this workspace's download client built?" a second time. Consumers now call this and need no `reqwest` dependency of their own ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
