Fixed
- The "no instructional content is installed" errors name `tm content update` as the remedy, with `tm content install --from <bundle.tar.gz>` as the offline alternative. A new `AgentContentError::FetchFailed` reports a failed first-use fetch the same way (#9396).
