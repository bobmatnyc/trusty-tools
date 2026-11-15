Fixed
- The exhaustive child-repo scan refuses a symlink whose target folds `..` past a dangling in-root symlink, which a later `mkdir` could turn into a path out of the root (#10473).
