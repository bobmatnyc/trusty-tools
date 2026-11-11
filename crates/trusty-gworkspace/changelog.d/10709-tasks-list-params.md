Added
- `manage_tasks` action `list` accepts optional `show_hidden`, `show_deleted`, `max_results` and `page_token`, sent to Google Tasks as `showHidden`, `showDeleted`, `maxResults` and `pageToken` only when supplied (#10709). A `list` without them sends the same request as before.
- The `list` response adds `next_page_token` beside the API's `nextPageToken` when another page exists, so a caller can page through a list (#10709).
