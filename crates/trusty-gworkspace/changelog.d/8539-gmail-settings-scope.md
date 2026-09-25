Fixed

- **The OAuth consent flow now requests `gmail.settings.basic`**, so
  `manage_gmail_filters` and `manage_gmail_settings` no longer fail with
  `403 ACCESS_TOKEN_SCOPE_INSUFFICIENT`. A profile authorized before this
  release keeps its old grant and must re-run
  `trusty-gworkspace-mcp setup --profile <name>` once
  ([#8539](https://github.com/bobmatnyc/trusty-tools/issues/8539))
