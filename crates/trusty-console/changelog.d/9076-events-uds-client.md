Added
- Console subscribes to the trusty-events bus over its Unix socket (`events_socket_path()`). It resumes after a hang-up with the last event's `seq` and id, resubscribes from 0 and drops duplicates by event id when the bus refuses a resume, passes gaps on, and backs off between attempts. A quiet bus does not end the subscription (#9076).
- `GET /api/console/events/status` reports the bus client's state, last error, retry delay and gap, duplicate and resync counts; `200` when connected, `503` otherwise. `/health` does not depend on the bus (#9076).
- New public module `events_uds`: the client, the `<seq>:<uuid>` resume token an SSE `id:` carries, and the in-process relay the event routes will read (#9076).
