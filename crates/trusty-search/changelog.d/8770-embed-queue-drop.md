Fixed
- A deferred-embed catch-up job whose task was dropped before it claimed its turn — its runtime shut down, or it was aborted — stayed at the head of the size-ordered queue, and every later index's embed pass waited behind it forever. A dropped job now leaves the queue at any point, and the next job runs (#8770).
