Changed
- The citation checks read `[doc: …]` as a bracket citation, not as free-text quotes: its excerpt is no longer matched against the diff, and a `[doc:]` citation that does not resolve in the docs read at the PR head withholds its finding (#9193).
