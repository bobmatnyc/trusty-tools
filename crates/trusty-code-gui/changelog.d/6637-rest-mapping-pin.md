Changed
- The bridge's status-table guard pins its rows to a written-out copy of the daemon's REST mapping, because #6637 slice 2c deleted the daemon's `serve::rest` layer it used to import; the mapping itself is unchanged ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637))
