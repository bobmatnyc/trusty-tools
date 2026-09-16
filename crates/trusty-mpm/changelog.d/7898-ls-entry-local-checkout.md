Fixed

- `tm ls` free-text project entry sends the project's local checkout to the create call instead of its clone URL, which ADR-0055's daemon refuses. A project that is not cloned on this host is now refused up front with the same `git clone <url> <path>` instruction the registered-row path prints, so a failed create no longer leaves a registration behind ([#7898](https://github.com/bobmatnyc/trusty-tools/issues/7898))
