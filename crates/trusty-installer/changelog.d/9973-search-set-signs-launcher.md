Changed

- `tctl install trusty-search` and `tctl sign trusty-search` now sign `trusty-embedderd-py` with `trusty-search` and `trusty-embedderd` (identifier `com.trusty.trusty-embedderd-py`). The launcher has shipped with trusty-search since #9468; before this change it stayed ad-hoc signed. A new test fails when a signing set and the binaries its crate installs disagree (Refs [#9973](https://github.com/bobmatnyc/trusty-tools/issues/9973))
