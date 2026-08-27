Fixed

- The `tga` stub the run tests install writes its invocation log to an absolute
  path under the test's work directory, and `invocations()` panics when that log
  is absent instead of reading it as an empty list. The stub used to append to
  `invocations.txt` relative to the child's own working directory — which only
  `run::child::spawn_tga` sets — so the same script installed behind
  `trusty-analyze` and `trusty-review` wrote into whatever directory the test
  binary started in, and every `assert_eq!(invocations(&work).len(), n)` then
  compared zero against a count nothing had recorded. Running the suite from the
  crate directory put the log in the checkout, which is how 49 blank lines of it
  were committed and then ignored (`f3ec62545`); that `.gitignore` entry is
  removed with the leak (#6293)
