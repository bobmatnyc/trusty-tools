Added

- The technical due-diligence report gains three new sections: "Code Quality &
  Architecture" and "Security Posture" re-project complexity, LoC/tech-stack,
  and RED/AMBER lint-tool findings already loaded for other sections —
  Security Posture is the promoted, now actually-filled, successor to the old
  §6.1 Security Violations table (previously unfilled template scaffolding).
  "Performance & Scalability" states, in fixed text never touched by
  synthesis, that no performance data source exists and what an assessment
  would require.
- A "Key Facts" block ahead of the executive summary frontloads codebase
  density (LoC, file count, languages) and a merged complexity profile —
  deterministic, never LLM-touched. Author count, work-volume estimate, and
  monthly trajectory rows render as named gaps until the authorship artifact
  lands.
- The executive summary carries a deterministic jump-list linking to every
  section actually present in the rendered document — never a link to a
  section a custom template omitted.
- Two new LLM narrative slots, `code_quality_summary` and `security_summary`,
  join the existing synthesis call and inherit its numeric guardrail. A
  template may override either section's voice via
  `<!-- instruct:code_quality_summary ... -->` /
  `<!-- instruct:security_summary ... -->`, same as the existing three.
- The default narrative voice (executive summary, top risks, and the two new
  slots) is now explicitly balanced/adversarial: acquirer-side, skeptical of
  risk, evenhanded about genuine strengths, never promotional.
- The `report-technical-dd-cast.md` template variant intentionally does not
  get the new Code Quality & Architecture / Security Posture / Performance &
  Scalability sections, the Key Facts block, or the Contents jump-list yet —
  porting them to CAST's own methodology and health-factor voice is deferred
  to follow-up work tracked on #6004.
