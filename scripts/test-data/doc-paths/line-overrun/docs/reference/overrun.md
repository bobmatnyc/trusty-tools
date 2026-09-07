# overrun

`docs/reference/target.md:2` names a line that exists.

`docs/reference/target.md:900` names one that does not. The file resolves, so
the reader still lands somewhere useful — that is a WARN, and the gate still
exits 0.
