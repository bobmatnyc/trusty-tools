Fixed

- Action-event spool replay no longer deletes a spool file before its events reach the bus. A replayed file stays on disk, locked, until its events are written to trusty-events or spooled again at stop, so a process SIGKILLed between the take and the flush loses none of the backlog. A crash in the short window after a write and before the delete can replay those events once more; they keep their event ids, which trusty-events deduplicates (Refs [#10467](https://github.com/bobmatnyc/trusty-tools/issues/10467))
