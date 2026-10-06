Fixed
- `tm hook --pm-guard` reads a here-document a shell runs even when its terminator is missing, so a delete nested in it no longer passes the rm-root floor (#9180).
- `tm hook --pm-guard` recognises a shell on a here-document operator line when its name is glued to `<<`, opened by a paren, a separator or a substitution, quoted, or a parameter such as `$SHELL`, and reads that body as commands (#9180).
- `tm hook --pm-guard` joins `\`-newline line continuations as the shell does: a continued command stays one segment, its joined spelling is judged too, and a continuation that splits a word, a `$(` opener or a substitution body no longer hides what runs (#9180).
- `tm hook --pm-guard` refuses a here-document it cannot place — a continuation that joins two `<`, follows a `#` on the operator line, or moves an unquoted body's end — and the rm-root floor denies as unresolved when the here-document scan gives up on a command that names a delete verb (#9180).
- The PM's forbidden-verb rules now judge each `$(…)` and backtick an unquoted here-document body expands, a nested one included (#9180).
