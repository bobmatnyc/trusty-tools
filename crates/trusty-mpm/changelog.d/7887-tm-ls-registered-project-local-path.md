Fixed
- `tm ls` new session: a registered project now starts in its directory under the projects root (`<repos_root>/<owner>/<repo>`) instead of sending the project's GitHub URL, which the daemon refused under ADR-0055. A project with no checkout there is refused in the picker, naming the expected path and the `git clone` that creates it (#7887).
