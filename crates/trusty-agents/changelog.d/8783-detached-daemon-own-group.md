Fixed
- The background daemon `tagent --service start` spawns now starts in its own session, so a group kill or Ctrl-C aimed at the calling CLI no longer kills it (#8783).
