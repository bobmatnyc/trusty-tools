Changed
- The summary in `review_body` is built from the verified result and never
  from the reviewer's prose: one sentence, then one line per surviving finding
  with its severity, `file:line` and title, highest severity first. A withheld
  or refuted finding is never named in it, and the withheld count stays in the
  "N findings withheld" headline above it. With no surviving finding the
  sentence names why: the reply did not parse, there was no reply, every
  finding was withheld from an approving or a rejecting review, or the
  reviewer raised none. The synthesis call still runs on the map-reduce path
  and still sets the verdict and grade (#9310).
