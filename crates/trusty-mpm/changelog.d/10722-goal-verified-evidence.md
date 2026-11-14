Fixed
- The Session Manager goal store refuses to store or update a linked session as `Verified` without non-blank evidence, with a typed `VerifiedWithoutEvidence` error that leaves the goal unchanged in memory, in the palace and in the cache (#10722).
- A state change now replaces the link's evidence with the update's evidence, and an update with no evidence clears it, so old evidence can no longer satisfy a later `Verified` (#10722).
- A goal persisted by an older build with an evidence-less `Verified` link still loads, but that link reads as `Running`, so the goal cannot close as `Done` through it. Progress, the `Done` gate and the delegation report count a link only when it is `Verified` and carries evidence (#10722).
