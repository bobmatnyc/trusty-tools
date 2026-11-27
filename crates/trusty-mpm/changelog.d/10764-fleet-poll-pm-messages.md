Fixed

- The seeded Architect `fleet-poll.py` reads `inbox/pm-messages.jsonl` each cycle through `pm_intake.py`, so a queued question, blocker, done or urgent message wakes the Architect within one poll interval. A poll state from before this change starts at the queue's end, so old messages do not wake it. The seeded `pm_intake.py` prefixes an outside MCP sender's wake text with `[OUTSIDE MCP sender=<sender>, not the owner; id <id>]`, and `ack`/`ack-through` take `--no-reply-needed` (Refs #10764).
