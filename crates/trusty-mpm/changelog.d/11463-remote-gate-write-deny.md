Security
- `tm hook --pm-guard` now refuses any PM or agent write to `~/.ssh/authorized_keys` or `~/.trusty-tools/trusty-common/config.yaml`, through the edit tools or Bash (redirects, `tee`, `cp`, `mv`, `sed -i`, `ln`, `rm`, `truncate`, `chmod`/`chown`/`chgrp`), in `~`, `$HOME` and absolute spellings. Reads, `~/.ssh/known_hosts` and other trusty config files stay writable.
- `chmod`, `chown` and `chgrp` operands are now judged against every trust anchor.
