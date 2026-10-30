Fixed

- Action-event spool replay no longer deletes a spool file before its events reach the bus. A replayed file stays on disk, locked, until its events are written to trusty-events or spooled again at stop, so a process SIGKILLed between the take and the flush loses none of the backlog (Refs [#10467](https://github.com/bobmatnyc/trusty-tools/issues/10467))
- A start replays no more spool files than its push buffer holds; newer files stay on disk for a later start. A file whose replayed events the buffer may have evicted is kept, never deleted
- Delivery is at-least-once: a file is retired whole, so a crash after part of it was sent replays all of it, and a kept file replays in full. trusty-events drops a repeated event id only while the first copy is still in its in-memory ring (8192 entries, not reloaded after a trusty-events restart); exactly-once across a crash needs bus acknowledgements and is out of scope
