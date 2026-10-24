Fixed
- Bare `tm` in a checkout with a non-GitHub origin (GitLab, Gitea, Bitbucket) now names the manual route in both its notice and its error: `tm connect <repo root>`, which runs the session directly in the live checkout, not in an isolated managed clone. It used to say only "use an explicit `tm` subcommand". `tm` still refuses and exits non-zero (#10255).
