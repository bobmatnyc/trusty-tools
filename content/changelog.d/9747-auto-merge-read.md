Fixed
- `version-control` and `tm-workflow` read the auto-merge setting with `gh api repos/<owner>/<repo> --jq .allow_auto_merge`. The previous `gh repo view` field is rejected by gh 2.98, so every read failed. An empty answer means the token cannot read the setting and counts as `false` (#9747).
