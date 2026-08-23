Fixed

- A reachability rewrite can no longer ship ungrammatical debris ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - a replacement that opens with a determiner now takes the article in front of it, so `by a remote attacker` becomes `by any local process` rather than `by a any local process` — the last report's Security Posture lead shipped exactly that
  - every rewritten sentence is read for grammar debris before it may ship: two adjacent determiners, or a contrast whose two sides describe the same reachability, send the field down the reject-and-disclose path instead of out to a reader
- A corrected-wording disclosure now cites the §5.1/§5.2 number of the finding it is about ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the withheld lines already carried "section 5.1, RED finding 2" while the corrected-wording line beside them named its finding by title only; the grounding check now hands that finding to the reporter, which resolves it to the rendered number
