Fixed

- A console Config save refuses a `~/.trusty-tools/trusty-mpm/config.yaml` it cannot parse, such as one with `auto_resume: maybe`. The refusal names the file and says it does not parse; it does not echo the file's content, so a token in `log_drain.secrets` never reaches the caller or a log. It used to read such a file as all defaults and then overwrite it, `channels:` section included. The file is now left unchanged. Refs #8454
