Fixed
- `memory_recall_all` skips palaces that hold no drawer without opening them, and its response reports `palaces_searched` and `palaces_skipped`. On a 104-palace sandbox copy of the live estate, 61 palaces are skipped and the median call drops from 29.4 s to 7.9 s with the same top 5 (#9141).
