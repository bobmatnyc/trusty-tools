Fixed
- A checkout with no `origin` remote now resolves its secrets scope from `git config --local trusty.secrets.repo <owner>/<repo>`. Only the checkout's own `.git/config` is read, and an `origin` remote still wins. With neither, the `project_unresolved` error names the command to run ([#11346](https://github.com/bobmatnyc/trusty-tools/issues/11346)).
