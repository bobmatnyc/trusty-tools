Added
- `tm memory rename <old> <new> [--replace-empty]`: move a trusty-memory palace to a new id over the daemon socket. It checks the daemon protocol first, reports a daemon that predates `palace_rename`, and prints the daemon's reason for a refusal (#9544).
