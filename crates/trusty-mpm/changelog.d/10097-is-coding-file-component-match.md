Fixed
- Source files whose path contains `history`, `cache`, `preferences`, `cookies`, `.log` or `.lock` inside a name (for example `compress/history.rs`) are no longer dropped from the hook event buffer and the trusty-events bus. The noise filter now matches path components and file names, and counts the events it drops. (#10097)
