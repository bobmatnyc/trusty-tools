Fixed

- BASE-ENGINEER's escape-sensitive edits section gains a dedicated bullet for numeric escapes (`\x00`, `\uXXXX`, `\0`, octal `\NNN`), with a concrete example of the failure (NUL byte injection) and the detection method (`od -c <file>`), closing a documentation gap that allowed the same corruption to recur undetected in live verification (Refs #7480).
