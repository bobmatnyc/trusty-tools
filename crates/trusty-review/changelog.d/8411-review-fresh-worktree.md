Fixed
- A review in a freshly provisioned git worktree no longer hard-skips until a manual reindex: when the worktree has no trusty-search index of its own, the review uses its main checkout's index (#8411).
- A checkout no index covers (for example a fresh clone) now gets a review of the diff alone, labelled DEGRADED with the missing index named, instead of a skip against the unregistered `main` default. Surfaces that require search still skip (#8411).
