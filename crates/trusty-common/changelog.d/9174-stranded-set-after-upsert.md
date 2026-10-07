Fixed
- The vector index keeps its list of unreachable drawers current after each write. A write that pushed an existing drawer out of its only neighbour list left that drawer unreachable by graph search until the palace was reopened; on palaces above 24,576 drawers it was missing from recall in the meantime. Each write now re-tests the drawers it pushed out and scans any it stranded (#9174).
