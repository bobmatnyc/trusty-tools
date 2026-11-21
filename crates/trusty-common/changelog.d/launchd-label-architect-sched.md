Added
- `launchd_labels::ARCHITECT_SCHED` registers `com.trusty.architect-sched`, and the stray-label scan exempts the leaf crate `trusty-architect`'s plist renderer, which cannot import it (#9171 A1).
