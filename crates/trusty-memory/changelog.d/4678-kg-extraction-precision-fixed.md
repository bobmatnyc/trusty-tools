Fixed

- **KG pattern extraction no longer turns function words into entities
  (issue #4678).** A `PATTERN_TABLE` marker hit (`is a`, `works at`, `uses`,
  `depends on`) took the token on each side as subject and object with no test
  applied, so "calling them is a no-op" asserted `them --is-a--> no-op` into the
  live graph. Tokens are now screened by `is_stop_token`: a closed-class
  function word (article, pronoun, preposition, conjunction, copula, auxiliary,
  discourse adverb) or anything shorter than three characters is refused, and
  refusing either side drops the whole triple rather than half of it. Short
  names this workspace actually discusses — `Go`, `C`, `C#`, `AI`, `KG`, `PR`,
  `CI`, plus the crate aliases `tm`/`ts`/`tc`/`ta` — are allowlisted past the
  length floor, so the filter costs no recall on them. The filter is lexical
  and judges one token at a time, so it does not reach a bad triple whose two
  tokens are both ordinary words (`squash --is-a--> ancestor`); that residue is
  pinned by a test rather than left unstated.
