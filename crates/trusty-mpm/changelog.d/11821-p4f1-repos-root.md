Fixed
- A relative `TRUSTY_MPM_REPOS_ROOT` is refused with `InstallPathsError::Relative` naming the value; it is no longer resolved against the cwd. Base clones refuse, and the readers log a warning and use the workspace root (#11821).
