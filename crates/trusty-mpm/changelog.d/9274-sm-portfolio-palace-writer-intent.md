Fixed
- The Session Manager palace (`sm-memory`) and the portfolio palace (`manager-memory`) open with writer intent, as the sole owners of their roots, so an existing format-0 palace migrates to format 1 at open instead of being refused with `FormatNeedsMigration` on every start (#9274).
