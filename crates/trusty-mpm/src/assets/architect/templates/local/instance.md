# Instance facts

Facts about this installation that the Architect finds and keeps current.
Read this file at the start of every pass. Rewrite an entry in place when it
changes, and label each fact **reported** or **verified** with its date.
The owner's own instructions go in `AGENTS.md`, never here. The Architect
edits `AGENTS.md` only when the owner orders a specific edit.

## Host

- Machine: <model, cores, memory>
- Builder slots: <how many concurrent builds the host carries>
- Load bands: <the load at which new builders hold or stop>

## Build hosts

- <name>: <what runs there and how it is reached>

## Accounts

- <service>: <which account for which work; never a secret, locations only>

## Services

- trusty-memory: <palace names in use, health check>
- trusty-search: <index names in use, health check>
- The Architect's own palace and index: `architect`

## Known quirks

- <date> <quirk and its workaround> (reported|verified)
