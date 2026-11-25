Changed

- The crate's two ports of `ilio.py`'s `SECRET` pattern are now one, `il::ilio::secret::looks_secret`, used by both `ilio check` and the IL packet path. It catches the union of what the two ports caught, so each path now also refuses the few control-character and Unicode-case-folding inputs only the other port caught before (Refs [#10571](https://github.com/bobmatnyc/trusty-tools/issues/10571))
- `trusty_architect::FakeTmux` is now a `ScriptedTmux` that answers every verb with an empty stdout. Its type name, `new`, `Default`, `calls` and `Tmux` impl are unchanged; only its `Debug` text differs (Refs [#10571](https://github.com/bobmatnyc/trusty-tools/issues/10571))
