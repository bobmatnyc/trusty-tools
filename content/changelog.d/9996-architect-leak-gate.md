Added

- The Architect leak gate (`scripts/check_architect_subproject.sh`, and the new `scripts/check_content.py leaks`) now rejects owner names, GitHub orgs outside an allow-list, `.local`/`.lan`/`.internal`/`.ts.net` hostnames, Slack ids and non-example emails, over `content/instructions/architect/**` as well as the Python sub-project and the shipped assets. Both tools read one pattern file, `scripts/architect_leak_patterns.txt`, and fail closed on a missing pattern file, a missing scan root, an empty pattern or a grep error (#9996).
