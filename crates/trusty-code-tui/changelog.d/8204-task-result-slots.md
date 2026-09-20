Added
- `ReplEvent::TaskResult` renders a finished task into labelled scrollback slots — summary, changed files one per line, test counts, captured test output — instead of one blob of prose, and keeps the report per agent on `ReplApp::finished_tasks` (#8204).
