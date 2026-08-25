Fixed
- A repository that failed now leaves a trace in the return package. Each one
  ships its child's own log as `failures/<stem>.log`, copied through the same
  symlink, hardlink and credential guards every other member goes through, and a
  generated `failures/index.md` names the repository, what went wrong, how long
  it ran, and the gaps it stated before it stopped. A failure with no log says
  so rather than being absent. Before this a failed target left nothing at all —
  "failed" and "never attempted" were the same observation from the outside, and
  diagnosing either meant re-running the sweep on the machine that had it.
- The generated members are scanned for the engagement's credentials before they
  are written, rather than trusted because this crate wrote them. The failure
  record quotes reason strings, and a reason can carry text a child produced.
