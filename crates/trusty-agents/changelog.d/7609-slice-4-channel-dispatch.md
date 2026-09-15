Changed
- Inbound dispatch is one selector shared by every channel provider: an assistant's own channel, a global channel naming it in `route_to`, or its legacy absorbed `[[listeners]]` binding — exactly one of the three wakes it per event, so a backfilled `route_to` and the binding it came from never both fire (#7609).
- A global `[[channels]]` entry now fans out to the assistants it names in `route_to`, for Slack and Telegram as well as Gmail; the two-stage opt-in is no longer the required path.
- A per-assistant binding that overlays an account-wide global channel is stored in the assistant's channels file instead of being left behind in `agent.toml`, and the startup sweep backfills `route_to` for every such pair.
- The Gmail channel wake carries the assistant's `events/<connector>.md` instructions, which previously reached only the `[[listeners]]` wake path.
- A `route_to` naming an unknown assistant, an unreadable assistant roster, and a channel whose provider has no adapter are each logged and claim nothing, instead of being absorbed into a silent no-op.
