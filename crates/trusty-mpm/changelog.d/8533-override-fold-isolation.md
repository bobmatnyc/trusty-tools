Fixed
- A `CLAUDE.md` override body ending in an unclosed `<!--` no longer hides the prompt text after its section. Each section is folded on its own, so the comment ends with the body (#8533).
- A `CLAUDE.md` override body or project output style that leaves a code fence open no longer turns the text after it into code: the fence is closed where the body ends. The fold now pairs `~~~` and four-backtick fences by character and length (#8533).
