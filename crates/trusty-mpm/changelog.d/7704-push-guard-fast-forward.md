Changed
- The cross-branch `pre-push` guard now permits a push onto a differently-named
  existing branch when it is a FAST-FORWARD of that branch, with no
  `TM_ALLOW_CROSS_BRANCH_PUSH=1` override — a fast-forward discards none of the
  destination's lineage. A push that does not descend from the destination's
  current tip stays refused, a rebased `--force-with-lease` included, and the
  refusal now says which exemption the push failed. The ancestry test fails
  CLOSED: a destination tip that cannot be resolved locally even after fetching
  that one ref is refused rather than waved through (#7704).
