Fixed

- ADR-0044's main-checkout write boundary now sees a `Bash` write made
  through `tee <path>`, inside a command substitution (`$(…)`, a backtick, a
  double-quoted `"$(…)"`, `>(…)`) or inside a subshell `( … )`, and denies it
  when the file is source in a main checkout; each used to be allowed (refs
  [#8730](https://github.com/bobmatnyc/trusty-tools/issues/8730))
  - a write the guard finds but cannot delimit — an unclosed `$(`, `(` or
    backtick, a `tee` whose arguments do not lex, nesting past the depth cap —
    is now refused instead of allowed
  - every redirect in a segment is judged, so `> notes.md > src/lib.rs` no
    longer hides the second file
  - a `>&word` redirect is read as the file write it is (bash opens `word`
    for stdout and stderr); only a descriptor word (`2>&1`, `>&-`, `>&2-`)
    is a copy. zsh's `>&|`/`>&!` always name a file
- The credential-print rule now refuses a `find-generic-password -w` whose
  stdout zsh's clobber spellings send to the terminal (`>!/dev/tty`,
  `>>!/dev/tty`, `>!/dev/stdout`, `>&!/dev/tty`); the `!` stayed on the
  target, which then read as a file. A `>|` to an ordinary file is no longer
  refused as unreadable (refs [#8730](https://github.com/bobmatnyc/trusty-tools/issues/8730))
- A here-document written into the session scratchpad is no longer refused as
  a source write when its body holds an apostrophe (`it's`) beside
  redirect-shaped prose; the body is read as data, as it already was without
  the apostrophe (refs [#8111](https://github.com/bobmatnyc/trusty-tools/issues/8111))
- The `ls`/`cp`/copy-script/delete refusals in an unpacked `git archive` tree
  under the scratchpad (refs [#8571](https://github.com/bobmatnyc/trusty-tools/issues/8571))
  are not changed here: `tm pm-guard` already allows them for the PM and for
  writing agents, and the refusal a read-only agent meets is the #8439
  read-only allowlist, whose scope is an open decision
