Added

- **`manage_drive_file` gains an `update` action** that replaces an existing
  Google Doc/Sheet/Slides file's content in place, keeping the file id, its
  share link, its permissions, and its revision history. It PATCHes the source
  bytes to `/upload/drive/v3/files/{id}?uploadType=media` (or `multipart` when
  `name` is supplied, so a rename rides along), and refuses any target that is
  not one of the three Google editor types with a structured error naming the
  actual `mimeType` — it never falls back to creating a new file
  ([#6685](https://github.com/bobmatnyc/trusty-tools/issues/6685))
