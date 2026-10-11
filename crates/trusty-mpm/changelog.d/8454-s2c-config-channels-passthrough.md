Fixed

- A console Config save no longer deletes the `channels:` section of `~/.trusty-tools/trusty-mpm/config.yaml`. `TrustyToolsConfig` now carries `channels` as an opaque value, so the save writes it back unchanged, and a valid section no longer logs an "unrecognised key" warning at every load. A misspelt top-level key such as `channles:` is still reported. Refs #8454
