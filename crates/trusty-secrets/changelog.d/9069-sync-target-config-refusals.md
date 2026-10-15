Security
- A tracked project file's `secrets.github_actions:` or `secrets.vercel:` section is refused with `tracked_cli_setting_refused`, so a repository cannot aim a push at a destination of its choosing; a sync target named as the project `secrets.backend` or the machine `secrets.default_backend` is refused with `config_invalid` (Refs #9069).
