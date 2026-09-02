Changed

- `fs_browse`'s registry `repo_url` parse delegates to
  `trusty_common::github_path::parse_remote_url` instead of splitting the URL
  itself (#6657). A GitLab-style subgroup path is still rejected, so a
  registry entry can never fabricate an owner containing a slash. A
  port-qualified host (`https://git.example.com:8443/owner/repo`) now resolves
  to `owner/repo` rather than being rejected: it was rejected only because the
  local split treated the port's colon as the host/path boundary, and the shared
  parser does not.
