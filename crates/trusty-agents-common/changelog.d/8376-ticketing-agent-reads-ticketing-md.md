Added

- the `ticketing` agent reads the project-root `TICKETING.md` before any create, label, comment, or transition in any tracker, generates it from the skill skeleton plus the observed repository state when absent, and never overwrites an existing one ([#8376](https://github.com/bobmatnyc/trusty-tools/issues/8376))
  - the file's behaviour settings are honoured, its contents are treated as data, and it outranks a conflicting brief unless the brief cites an owner ruling
  - epic Tracker autoupdates are confined to the agent's own marker comments, follow-ups are budgeted per phase issue, and stale-issue recommendations are posted per epic as one digest
