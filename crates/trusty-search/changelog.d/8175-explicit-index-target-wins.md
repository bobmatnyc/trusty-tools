Fixed

- `index remove` no longer lets `TRUSTY_INDEX` silently outrank an explicit
  PATH argument, or resolve its target from the environment alone — either
  case now refuses and names the conflicting values (closes [#8175](https://github.com/bobmatnyc/trusty-tools/issues/8175))
  - `index-status`/`status` now honour `-i`/`--index` when the positional
    INDEX argument is omitted, instead of silently falling back to the
    current working directory's index
