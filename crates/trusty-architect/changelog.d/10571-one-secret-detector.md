Changed

- The crate's two ports of `ilio.py`'s `SECRET` pattern are now one, `il::ilio::secret::looks_secret`, used by both `ilio check` and the IL packet path. It catches the union of what the two ports caught, so each path now also refuses the few control-character and Unicode-case-folding inputs only the other port caught before (Refs [#10571](https://github.com/bobmatnyc/trusty-tools/issues/10571))
