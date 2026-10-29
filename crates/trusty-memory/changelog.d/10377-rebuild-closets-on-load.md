Fixed

- A palace loaded from disk (restart, idle evict and reload, restore) now builds its closet keyword index on open. Before, the index stayed empty until the next write or dream cycle, and every recall lost the 0.15 closet tag boost in that window (#10377).
