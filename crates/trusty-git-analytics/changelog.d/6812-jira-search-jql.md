Fixed

- `tga jira sync` posts to `/rest/api/3/search/jql`. Atlassian removed
  `/rest/api/3/search` and answers it with HTTP 410 (CHANGE-2046), so every
  sync against a Jira Cloud site left `fact_ticket_transitions` and
  `fact_jira_comment_detail` empty and wrote no cursor. The replacement
  endpoint paginates by an opaque `nextPageToken` instead of `startAt`,
  reports no `total`, takes `expand` as a comma-delimited string rather than
  an array, and may return a page shorter than the requested `maxResults`
  while more pages remain — so both search walks now end on the absent token
  and never on page length (#6812).
- **Breaking:** the public `collect::jira::paging` API paginates by token, which
  is why this release is 8.0.0 and not a patch. `PageRequest.start_at: u64` is
  replaced by `next_page_token: Option<String>`, and `KeysetPager::record_page`
  takes the server's continuation token in place of the requested page size
  (#6812).
