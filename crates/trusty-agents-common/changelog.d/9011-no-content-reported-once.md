Added
- `report_not_installed` logs "no instructional content is installed" as one ERROR per process and tells the caller to add no line of its own; `mark_not_installed_reported` records that the caller printed it itself; `AgentContentError::is_not_installed` names the case (#9011).
