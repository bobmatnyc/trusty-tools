Documentation
- The action-event settle doc now states that under load the 200 ms signal budget can end before the spool write lands; the signal path then reports an upper-bound `lost` count while the settle still spools the events (#10412).
