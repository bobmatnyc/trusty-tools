Documentation
- Added a File-Size Precheck rule to `BASE-AGENT.md`: before the first edit
  to a production source file, every agent now measures its current size
  with the project's cap tool and plans a split up front when the planned
  addition would push it over the cap.
