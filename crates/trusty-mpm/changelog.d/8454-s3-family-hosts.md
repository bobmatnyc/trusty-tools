Fixed
- The `tm hook --egress-guard` Bash arm now denies a command that reaches `chat.google.com` (Google Chat) or `notion.site` (Notion), or a subdomain of either. Both hosts were missing from its family-host list.
