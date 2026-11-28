Fixed

- The shadow scheduler's idle alerts now follow `fleet-poll.py`: a not-idle read resets the idle epoch, and a changed idle screen raises its new `since-<epoch>-<screen>` alert at once. The window-5 grace and settle rules are gone (Refs [#11826](https://github.com/bobmatnyc/trusty-tools/issues/11826))
- `trusty-architect-sched parity` keeps the owner kinds the scheduler does not raise yet (`owner_feedback`, `owner_unpark_due`, `owner_command`, `owner_interactive`, `owner_effort`, `steer`) out of the match percentages and prints them as a separate known-absent count (Refs [#11826](https://github.com/bobmatnyc/trusty-tools/issues/11826))
