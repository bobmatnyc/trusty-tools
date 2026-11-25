Fixed
- The build-lease census test `a_real_foreign_compiler_is_counted` no longer fails on Linux when the fake compiler's libtest thread is sampled before it renames itself; it now counts only the fake compiler and its ancestors (Refs #11863)
