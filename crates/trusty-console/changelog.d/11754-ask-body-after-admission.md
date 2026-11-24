Fixed
- `POST /api/console/assistants/{id}/ask` reads the request body only after the request is admitted, so a request no gated listener admitted is refused with 403 before any of its body is read. The handler enforces the 64 KiB body cap itself: a larger body is still 413 and an unreadable body still 400 ([#11754](https://github.com/bobmatnyc/trusty-tools/issues/11754)).
