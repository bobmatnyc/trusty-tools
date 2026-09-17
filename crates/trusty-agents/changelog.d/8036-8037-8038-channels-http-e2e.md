Added

- `POST /api/channels` and `PUT /api/channels/{id}` declare or replace ONE
  global channel, so a client no longer has to echo the whole list back to add
  one — omit a channel from the whole-list `PUT` and it is deleted. Both take
  the same `revision` compare-and-swap and the same `channel_auth::ChannelWriter`
  gate as the whole-list write; an unauthenticated create is refused
  ([#8038](https://github.com/bobmatnyc/trusty-tools/issues/8038)).
- `POST /api/channels/{id}/inbound` injects one inbound event on a declared
  global channel into `agent_channels::inbound::receive_inbound` — the same path
  Slack, Telegram and Gmail take — so #7609's wake/dispatch behaviour is
  verifiable without a live provider credential. The response names the
  assistants that actually woke, the source that selected them
  (`global-route-to`, `assistant-channel`, `legacy-binding`), and the dispatch
  budget outcome. Gated by `ChannelWriter` like every other channel write
  ([#8036](https://github.com/bobmatnyc/trusty-tools/issues/8036)).
- A `stub` channel provider that needs no credential, admitted only when
  `TAGENT_STUB_CHANNEL` is set at process start. It is held in a second adapter
  table the registry consults ONLY behind that switch, so a default daemon
  resolves `stub` to no adapter at all: the provider cannot be saved to a
  channel, is absent from the providers listing the Channels UI reads, and wakes
  nobody. Sends are recorded in an in-process outbox the history read hands back,
  which makes `POST .../channels/{id}/send` and `GET .../channels/{id}/messages`
  exercisable end to end
  ([#8037](https://github.com/bobmatnyc/trusty-tools/issues/8037)).
- `ChannelAdapter::read` — prior messages on a bound destination, per provider.
  The default REFUSES rather than answering with an empty list, so a provider
  that claims readable history and implements none cannot render as a quiet
  inbox.
