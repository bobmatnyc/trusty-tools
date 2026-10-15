Fixed
- The Slack channel adapter's history read now reaches Slack as `GET conversations.history` with `channel` and `limit` in the URL query, through the trusty-channels client fix. Slack ignored those arguments when they were sent as a POST JSON body (Refs #9919).
