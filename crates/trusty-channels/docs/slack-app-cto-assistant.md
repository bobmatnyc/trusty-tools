# CTO Assistant — Slack App Manifest

This is the saved Slack app manifest for the **CTO Assistant** app (app_id `A0AMPRM4W0J`) in the Duetto workspace. The manifest defines OAuth scopes, bot user settings, socket-mode configuration, and MCP enablement for the native `slack-mcp` server that drives the app.

The manifest contains no secrets — only configuration and scopes. It can be used to recreate or update the app via Slack's "Create from manifest" flow at [api.slack.com/apps](https://api.slack.com/apps). Note that the user scopes include `search:read`, which is required for message search functionality via `slack_search_messages`.
