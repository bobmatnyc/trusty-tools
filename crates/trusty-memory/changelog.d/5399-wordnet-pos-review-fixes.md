Fixed

- The KG noun-phrase walk no longer crosses a line break. Production hands the
  extractor whole multi-line drawer bodies (`memory_remember`, `kg-rebuild`), so
  a walk with no newline boundary took its head from the next sentence:
  `trusty-search is a daemon\ncargo builds it` asserted
  `trusty-search --is-a--> builds`. Because the KG keeps one active triple per
  `(subject, predicate)`, a rebuild rewrote a correct stored object with the
  wrong one (#5399).
- A participle no longer heads a noun phrase. `WordNetPos::mask` now retries a
  regular `-s` / `-es` / `-ies` / `-ing` / `-ed` inflection against its base
  form, so `containing` resolves to the verb `contain` and
  `Each skill is a directory containing:` yields `skill --is-a--> directory`
  instead of `containing`. Plurals keep their noun sense, so `parsers` is still
  a valid head (#5399).
- `inside` and `outside` join the closed-class preposition list; WordNet records
  both as nouns, so without it they passed the part-of-speech check and
  continued a phrase they actually close. This is what ends the phrase in
  `tree is a comment inside crates/…/tests.rs`, which yields `comment` (#5399).
- A plural of a word ending in `e` resolves to that word rather than to the stem
  left by chopping `es` off it. `-es` was tried unconditionally before `-s`, so
  `notes` answered the adverb `not` and `sites` the verb `sit`, both of which
  end a noun phrase — `notes is a drawer` and `sites is a directory` yielded
  nothing at all. The order now follows English spelling: `-es` first only when
  the stem ends in a sibilant, so `attaches` still answers `attach` and not the
  noun `attache` (#5399).
