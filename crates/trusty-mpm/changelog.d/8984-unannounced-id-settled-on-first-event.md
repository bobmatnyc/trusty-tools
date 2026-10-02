Security

- A session whose `SessionStart` never reached the daemon (daemon down at
  start, no tm `SessionStart` hook, a pre-upgrade session) is now settled as
  unproven by its first in-session hook event, such as `PreToolUse` or
  `Stop`. A sibling's forged `SessionStart` for that id can no longer bind it
  as the delegation-repair owner. A legitimate socket `SessionStart` already
  in flight when that event arrives still binds. Start-up events
  (`InstructionsLoaded`, `ConfigChange` and the like) do not settle an id.
