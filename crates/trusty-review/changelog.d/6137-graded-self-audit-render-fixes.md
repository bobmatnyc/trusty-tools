Fixed

- The numeric guardrail now admits scaled-unit restatements of a large integer, so "1.55 million lines" for a measured 1,553,771 verifies instead of vetoing the whole field. It also admits the figures the report computes at render time and prints in its own sections (the investigation coverage percentage, the authorship trajectory average), which the synthesis prompt already quotes verbatim.
- Code Quality & Architecture reads LoC and primary tech through the same source precedence Key Facts uses, so a `--analyze` run — whose fetch leaves `loc`/`counts` to the scanner by design — no longer prints "not stated in source data" beside a §4.1 that renders the scan's figures.
- AMBER findings render their component path, so a complexity finding no longer reads "Extract the body of 'this function' (lines 23-512)" with no file named.
- Analyze data whose component paths fall outside the audited checkout is rejected as stale-index evidence with a named gap, instead of being stamped measured. An index is addressed by directory basename, so a second checkout of the same repository answers for the audited one.
- Every GREEN finding renders as its own topic line. The templates carried three fixed slots, so a run with 21 GREEN findings dropped 18 of them.
- Security Posture counts only the security dimension's findings, credits that dimension's clean signals, states its actual provenance (verified LLM investigation findings, code-hygiene scope), and prints one table rather than one per row.
- Executive-summary jump-list anchors match GitHub's rule (punctuation deleted, spaces mapped to dashes), so a heading containing `&` no longer links to an anchor that does not exist.
- An empty Dependency Inventory renders as a named gap naming what was or was not examined, never as "no manifest-declared dependencies were found". `[workspace.dependencies]` is now inventoried, which is where a cargo workspace root declares its dependencies.
- Performance & Scalability points at section 5 when the investigation raised scalability findings, rather than reading as though neither was assessed.
- A verified evidence quote is completed to the end of the line it ends on, so a mitigating clause on that same line cannot be cut away from the finding it qualifies.
