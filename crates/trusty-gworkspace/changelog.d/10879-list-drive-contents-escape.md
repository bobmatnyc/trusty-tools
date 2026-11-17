Fixed
- `list_drive_contents` escapes `folder_id` as a Drive query literal through `api::bulk::parents_query` (#10879), so a crafted ID cannot add a clause that lists another folder's files.
- The Drive walk's root check (#10746) applies to every `DriveWalk`, trusty-agents' okg drive ingest included; the `root` alias passes it (#10879).
