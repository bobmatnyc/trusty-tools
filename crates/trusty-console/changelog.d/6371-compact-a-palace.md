Added
- The Memory tab compacts a palace from its roster
  (`POST /api/console/memory/palaces/{id}/compact`), calling trusty-memory's own
  `palace_compact`. Two clicks, with a confirm step naming the palace, and the
  reclaimed counts reported from the daemon's answer (#6371).
