Fixed

- The investigation byte budget now derives from the file budget at 10 KiB per
  file unless the environment or the manifest declares one. `trusty-review`
  stops selecting at whichever of the two caps binds first, so raising
  `TRUSTY_AUDIT_INVESTIGATE_MAX_FILES` alone read fewer files than it asked for
  and reported nothing about it — a 120-file lap read 76 files because the fixed
  1.2 MiB cap bound first. An explicit `TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES`, or
  a declared `[report].investigate_max_bytes`, still wins outright.
- The default investigation budget is 240 files and 2.4 MiB per repository, up
  from 120 files and 1.2 MiB: 120 sampled about 1.8% of a workspace-sized
  repository, below the coverage a due-diligence report is expected to reach.
  The evidence caps follow it — 240 ranked paths and 34 files per dimension.
