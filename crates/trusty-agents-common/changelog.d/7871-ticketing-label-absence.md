Fixed

- The `ticketing` agent body now requires `gh label list -R <owner>/<repo>` in
  front of any claim that a repository lacks a label, and says what to do with
  each answer — use the existing label, or `gh label create <name>` and report
  what was created
  (refs [#7871](https://github.com/bobmatnyc/trusty-tools/issues/7871))
