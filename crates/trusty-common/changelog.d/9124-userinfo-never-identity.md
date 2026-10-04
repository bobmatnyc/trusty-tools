Security
- `parse_github_path`, `parse_remote_url`, `owner_repo_from_git_remote` and `repo_slug_from_git_remote` strip a remote URL's userinfo before deriving anything, so a token embedded as `https://user:<token>@host/x.git` no longer becomes the owner of a managed-checkout path, a palace id, a log line or an error (#9124). New `url_userinfo::strip_userinfo` and `url_userinfo::userinfo_end`.
