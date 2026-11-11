Fixed

- `tm doctor`'s `egress_guard` description, as `tm generate capabilities` renders it, now says the guard denies outbound-channel MCP tools of the native servers and outbound Bash egress to those hosts, and that the claude.ai connectors (`claude_ai_*`) are exempt except for their write tools in a channel-tainted session ([#10735](https://github.com/bobmatnyc/trusty-tools/issues/10735)).
