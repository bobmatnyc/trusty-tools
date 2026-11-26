Added
- `tm whats-new` prints the curated highlights of the installed release: a few lines per feature, each ending with the command to try. A release with no highlights file of its own shows the newest earlier one (#11003).
- `tm doctor` prints a one-line pointer to `tm whats-new` the first time it runs on a release, recorded in `whats-new.json` under the framework root. A store that cannot be read or written never fails doctor: the line shows again with a warning on stderr, and the release is not marked seen (#11003).
