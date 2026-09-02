Changed

- `extract_owner_repo_from_url` delegates to
  `trusty_common::github_path::parse_remote_url` instead of parsing the URL
  itself, and the second copy of it under `commands::deployments` is now a
  re-export of the first (#6657). The accepted forms are unchanged — HTTPS,
  scp-style SSH, `ssh://`, and `https://user@`, GitHub hosts only.
