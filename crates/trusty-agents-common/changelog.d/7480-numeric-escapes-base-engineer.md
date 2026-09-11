Fixed

- BASE-ENGINEER's escape-sensitive character list now names numeric escapes (`\uXXXX`, `\xXX`, octal `\NNN`) and states that the Edit/Write tool's own `new_string`/`content` argument is a direct-write corruption route, not only shell-routed edits (Refs #7480).
