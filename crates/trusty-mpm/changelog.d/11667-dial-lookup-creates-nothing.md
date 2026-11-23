Fixed
- `tm tui` socket lookups for Agents, Analyze, Review and Console no longer create the data directory, so a missing directory is not recreated at 0755 and refused by the 0700 trust check (#11667).
