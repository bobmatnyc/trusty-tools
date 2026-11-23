Fixed
- The bounded screenshot no longer refuses every capture from cua-driver 0.23.2, which reports `elements_complete: false` on a full walk. A tree now counts as complete when `returned_element_count` and `total_element_count` are both present and equal and `degraded` is false or absent; anything else still refuses with `screenshot_unverified` (Refs #11694).
