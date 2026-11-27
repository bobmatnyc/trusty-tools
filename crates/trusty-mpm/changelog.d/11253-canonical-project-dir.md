Fixed

- `tm project init`, `tm project info` and `POST /projects` canonicalize the project directory, so a symlink, a trailing slash or a case variant of one checkout gives one name, one record and one path. An unresolvable path is refused with a 400 that names it (Refs #11253).
