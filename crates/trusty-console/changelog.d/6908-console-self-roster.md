Added
- The console lists itself in the Services roster. `GET /api/console/services`
  now carries a `trusty-console` entry — status `running`, lifecycle `daemon`,
  version read from the binary — through the same generic row path as the other
  six members, with %CPU and RSS sampled from the console's own pid once a
  second like every resident daemon.
- The machine-status history snapshot carries `sse_client_count`: how many
  browser streams are attached to `/api/console/machine-status/stream` at the
  instant the snapshot was built, read live off the broadcast rather than kept
  as a second tally. `schema_version` is `4`. This counts browser streams only;
  it is not a DOC-73 bus subscriber count, and bus status, service connections
  and message rates remain unimplemented pending the bus ingest (#6460, #6862).
