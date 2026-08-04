Fixed

- memory TUI no longer hides palaces whose counts the daemon never measured — `palace_has_content()` now reads the `Option<u64>` accessors instead of the raw zeroed fields, so *unknown* counts keep a palace visible (rendered as `—`) while a measured-empty palace stays filtered out (closes [#4690](https://github.com/bobmatnyc/trusty-tools/issues/4690))
