Fixed

- The egress guard test `load_past_deadline_denies` no longer fails on a loaded CI runner. It now reads the clock around `check_with` itself, not before the fixture is built, and checks that the load's git deadline falls the kill margin before the wait ends. The hung loader now blocks on a channel, not a 2 s sleep. The guard's behaviour is unchanged: a load past its deadline still denies (#10735).
