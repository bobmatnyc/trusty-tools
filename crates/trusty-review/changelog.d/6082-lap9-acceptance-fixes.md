Fixed

- An unevidenced component's reach claim is now caught by vocabulary, not by a table of known phrases ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - `remote`, `network` and `internet` are read as tokens, so a hyphenated compound (`remote-execution`) and a morphological variant (`remotely`, `network-reachable`) are examined; the last report shipped "a critical remote-execution and privilege risk" because no rewrite pattern matched it
  - only CLAIM-shaped uses are withheld — the word must sit within three tokens of an access, attack, execution or exposure word — so "a transient network error" and "network and streaming logic" keep their field
- A Synthesis Status disclosure about a report-level paragraph now cites the §5.1/§5.2 number of the finding it is about ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the executive, code-quality, security and authorship summaries belong to no single finding, so their rejections quoted a finding title with no number; the grounding check now hands that finding to the reporter, which resolves it to the rendered number
