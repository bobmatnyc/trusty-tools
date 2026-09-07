Added
- `analyze.smells` rows carry a `smells` array naming what the detector found.
  The method selected chunks by running the detector and then discarded its
  output, so a row said "this chunk smells" without saying of what — the only
  other string on it, `match_reason`, is the SEARCH daemon's hit reason and
  says nothing about code quality. Consumers that group by smell had nothing to
  group by: the analyze dashboard's Smells view rendered "No smells detected for
  this index" over a corpus whose own quality card reported thousands.
  `quality::smelly_chunks_with_smells` keeps the detector output the existing
  `smelly_chunks` throws away; that function is unchanged, so no caller of it
  moves ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
