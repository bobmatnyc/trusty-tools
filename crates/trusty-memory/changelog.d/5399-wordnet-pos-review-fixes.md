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
- An unknown token may open a noun phrase but no longer joins one that already
  has a head, so a file path or code identifier cannot displace the real noun:
  `tree is a comment inside crates/…/tests.rs` yields `comment` (#5399).
- `inside` and `outside` join the closed-class preposition list; WordNet records
  both as nouns, so without it they passed the part-of-speech check and
  continued a phrase they actually close (#5399).
