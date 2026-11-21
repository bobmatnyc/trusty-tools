Fixed

- The issue audit counts a list marker only when whitespace follows it, so a `**bold**` paragraph, `-1 …` or `1.5x …` after `Acceptance criteria:` / `- TBD` no longer passes as a criterion; the acceptance row WARNs.
