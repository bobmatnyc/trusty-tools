Added
- `PushClient::drain_buffered` removes and returns every buffered frame, oldest first, so a producer can hand undelivered events to a fallback store at shutdown (#10412).
