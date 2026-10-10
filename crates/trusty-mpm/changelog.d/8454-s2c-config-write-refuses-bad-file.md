Fixed

- A console Config save refuses a `~/.trusty-tools/trusty-mpm/config.yaml` it cannot parse, such as one with `auto_resume: maybe`, and returns the parse error. It used to read such a file as all defaults and then overwrite it, `channels:` section included. The file is now left unchanged. Refs #8454
