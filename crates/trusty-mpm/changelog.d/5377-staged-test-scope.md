Changed

- `tm-workflow` gains a `Test Scope Widens by Stage` section, placed after the Sprint/Harden doctrine it refines ([#5377](https://github.com/bobmatnyc/trusty-tools/pull/5377))
  - unit tests run on the new or changed code only while developing, on the full test files that changed when merging, and on the full corpus only when publishing
  - merging widens to whole files, never to the whole repository — including cases you did not edit and any normally-skipped test in those files. A change to a public interface or to shared test infrastructure makes a dependent package's test files count as changed even though the diff never opened them
  - stated stack-agnostic, since `tm-workflow` governs every project: the section names no build-tool command
  - stage and rigour are named as separate axes that both bind — the stage decides how wide a gate runs, the project's risk labels and test ladder decide how hard it is applied and which gates run at all
  - a project may leave a full-corpus CI job off its required pre-merge contexts, since that job is a publish gate. A *failing* check still blocks the merge at every stage; only *pending* is tolerated
  - the hard line is restated in place: choosing the narrower scope is a blast-radius claim you must be able to prove, never licence to make a red gate green by deleting a test, marking it skipped, gating it out of the build, or excluding it from the run
