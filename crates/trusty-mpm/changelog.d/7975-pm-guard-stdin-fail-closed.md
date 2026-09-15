Fixed
- `tm hook --pm-guard` now denies a tool call whose stdin payload is empty, unreadable, still open after the read timeout, not valid JSON, or not a JSON object. It used to allow those calls. The deny reason names the failure (#7975).
