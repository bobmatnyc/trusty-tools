Added
- `log_rotation.keep` in `~/.config/trusty-search/config.yaml` sets how many rotated `stderr.log` archives newsyslog keeps (default 7, range 1 to 1000). `trusty-search doctor --fix` now writes the configured count, and keeps a count already in the installed conf when the config sets none, instead of resetting it to 7. A value outside the range is refused with an error (#10297).
